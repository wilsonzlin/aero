/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "adapter_context.h"
#include "backend.h"
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
#include "aero_virtio_snd_ioport_backend.h"
#endif
#include "portcls_compat.h"
#include "trace.h"
#include "virtiosnd_dma.h"
#include "virtiosnd_limits.h"
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
#include "virtiosnd_sg.h"
#endif
#include "wavert.h"

/*
 * Eventq handling:
 *
 * Aero contract v1 defines no eventq messages, but virtio-snd specifies
 * asynchronous PCM events (period-elapsed / XRUN). Newer device models may emit
 * these events; handle them defensively to avoid crashes and best-effort recover
 * from XRUN without relying on eventq for correctness.
 */

#ifndef KSAUDIO_SPEAKER_MONO
// Some WDK environments may not define KSAUDIO_SPEAKER_MONO; it maps to FRONT_CENTER.
#define KSAUDIO_SPEAKER_MONO SPEAKER_FRONT_CENTER
#endif

typedef struct _VIRTIOSND_WAVERT_STREAM VIRTIOSND_WAVERT_STREAM, *PVIRTIOSND_WAVERT_STREAM;

typedef struct _VIRTIOSND_WAVERT_STREAM_FORMAT {
    USHORT Channels;
    /* WAVEFORMATEXTENSIBLE container bits (wBitsPerSample). */
    USHORT BitsPerSample;
    /* WAVEFORMATEXTENSIBLE valid bits (wValidBitsPerSample). */
    USHORT ValidBitsPerSample;
    ULONG SampleRate;
    ULONG BlockAlign;
    ULONG AvgBytesPerSec;
    ULONG ChannelMask;
    GUID SubFormat;
    UCHAR VirtioFormat;
    UCHAR VirtioRate;
    /*
     * Timer/grid constraints:
     *
     * The WaveRT period timer uses an integer-millisecond periodic interval.
     * Many sample rates (e.g. 44.1kHz) do not have an integer number of frames
     * per millisecond. To keep the audio timeline accurate, period sizes are
     * constrained to multiples of MsQuantum such that:
     *
     *   frames_per_quantum = SampleRate * MsQuantum / 1000
     *
     * is an integer, and BytesPerQuantum is the corresponding byte count.
     */
    ULONG MsQuantum;
    ULONG BytesPerQuantum;
} VIRTIOSND_WAVERT_STREAM_FORMAT, *PVIRTIOSND_WAVERT_STREAM_FORMAT;

typedef struct _VIRTIOSND_WAVERT_FORMAT_ENTRY {
    /*
     * KSDATARANGE_AUDIO must be the first member so KSDATARANGE pointers passed
     * back from PortCls (MatchingDataRange) can be converted back to the owning
     * format entry via CONTAINING_RECORD().
     */
    KSDATARANGE_AUDIO DataRange;
    VIRTIOSND_WAVERT_STREAM_FORMAT Format;
} VIRTIOSND_WAVERT_FORMAT_ENTRY, *PVIRTIOSND_WAVERT_FORMAT_ENTRY;

typedef struct _VIRTIOSND_WAVERT_MINIPORT {
    IMiniportWaveRT Interface;
    LONG RefCount;

    PVIRTIOSND_BACKEND Backend;
    VIRTIOSND_PORTCLS_DX Dx;
    BOOLEAN UseVirtioBackend;

    KSPIN_LOCK Lock;
    PVIRTIOSND_WAVERT_STREAM RenderStream;
    PVIRTIOSND_WAVERT_STREAM CaptureStream;

    /*
     * Dynamically generated filter descriptor and supported-format tables built
     * from virtio-snd PCM_INFO.
     *
     * When unavailable (e.g. null backend / legacy build), the driver falls back
     * to the static fixed-format descriptor.
     */
    PCFILTER_DESCRIPTOR FilterDescriptor;
    PCPIN_DESCRIPTOR Pins;

    PVIRTIOSND_WAVERT_FORMAT_ENTRY RenderFormats;
    ULONG RenderFormatCount;
    PKSDATARANGE* RenderDataRanges;

    PVIRTIOSND_WAVERT_FORMAT_ENTRY CaptureFormats;
    ULONG CaptureFormatCount;
    PKSDATARANGE* CaptureDataRanges;
} VIRTIOSND_WAVERT_MINIPORT, *PVIRTIOSND_WAVERT_MINIPORT;

typedef struct _VIRTIOSND_WAVERT_STREAM {
    IMiniportWaveRTStream Interface;
    LONG RefCount;

    PVIRTIOSND_WAVERT_MINIPORT Miniport;
    KSSTATE State;
    BOOLEAN Capture;
    BOOLEAN HwPrepared;

    VIRTIOSND_WAVERT_STREAM_FORMAT Format;

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
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    /*
     * Optional tick coalescing:
     *  - WaveRT maintains a periodic timer for contract-v1 compatibility.
     *  - eventq PCM notifications can also queue this DPC as an additional wakeup.
     *
     * Keep a timestamp of the last processed tick so back-to-back DPCs don't
     * advance PacketCount/registers twice for the same period.
     */
    ULONGLONG LastTickTime100ns;
#endif

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

    // Capture (RX) in-flight tracking. Only used when Capture == TRUE.
    volatile LONG RxInFlight;
    ULONG RxPendingOffsetBytes;
    ULONG RxWriteOffsetBytes;
    KEVENT RxIdleEvent;
} VIRTIOSND_WAVERT_STREAM, *PVIRTIOSND_WAVERT_STREAM;

// Forward declarations for vtables.
static const IMiniportWaveRTVtbl g_VirtIoSndWaveRtMiniportVtbl;
static const IMiniportWaveRTStreamVtbl g_VirtIoSndWaveRtStreamVtbl;

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
static EVT_VIRTIOSND_RX_COMPLETION VirtIoSndWaveRtRxCompletion;

static VOID VirtIoSndWaveRtMiniport_FreeDynamicDescription(_Inout_ PVIRTIOSND_WAVERT_MINIPORT Miniport);
static NTSTATUS VirtIoSndWaveRtMiniport_BuildDynamicDescription(_Inout_ PVIRTIOSND_WAVERT_MINIPORT Miniport);
#endif

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
static VOID VirtIoSndWaveRtPcmXrunWorkItem(_In_ PVOID Context);
static VOID VirtIoSndWaveRtEventqCallback(_In_opt_ void* Context, _In_ ULONG Type, _In_ ULONG Data);
#endif

static __forceinline VOID VirtIoSndWaveRtInterlockedOr(_Inout_ volatile LONG* Target, _In_ LONG Mask)
{
    LONG oldValue;
    LONG newValue;
    for (;;) {
        oldValue = *Target;
        newValue = oldValue | Mask;
        if (InterlockedCompareExchange(Target, newValue, oldValue) == oldValue) {
            break;
        }
    }
}

static ULONG VirtIoSndWaveRtGcdUlong(_In_ ULONG A, _In_ ULONG B)
{
    /* Euclid's algorithm. */
    while (B != 0) {
        ULONG t = A % B;
        A = B;
        B = t;
    }
    return A;
}

static VOID VirtIoSndWaveRtFormatInitQuantum(_Inout_ PVIRTIOSND_WAVERT_STREAM_FORMAT Format)
{
    ULONG gcd;

    if (Format == NULL) {
        return;
    }

    gcd = VirtIoSndWaveRtGcdUlong(Format->SampleRate, 1000u);
    if (gcd == 0 || Format->BlockAlign == 0) {
        Format->MsQuantum = 1u;
        Format->BytesPerQuantum = Format->BlockAlign;
        return;
    }

    Format->MsQuantum = 1000u / gcd;
    Format->BytesPerQuantum = (Format->SampleRate / gcd) * Format->BlockAlign;
}

static __forceinline BOOLEAN VirtIoSndWaveRtShouldLogRareCounter(_In_ LONG Count)
{
    ULONG u;

    /*
     * Log the first few occurrences, then exponentially back off (powers of two).
     * This keeps always-on error logging from spamming if the device floods XRUN
     * notifications.
     */
    if (Count <= 4) {
        return TRUE;
    }
    if (Count < 0) {
        return TRUE;
    }

    u = (ULONG)Count;
    return ((u & (u - 1u)) == 0u) ? TRUE : FALSE;
}

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

static __forceinline UINT64
VirtIoSndWaveRtBackendBase(_In_ const VIRTIOSND_DMA_BUFFER* Buffer)
{
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    return (UINT64)(ULONG_PTR)Buffer->Va;
#else
    return Buffer->DmaAddr;
#endif
}

static BOOLEAN
VirtIoSndWaveRtChannelMaskForChannels(_In_ USHORT Channels, _Out_ PULONG OutChannelMask)
{
    if (OutChannelMask == NULL) {
        return FALSE;
    }

    switch (Channels) {
    case 1:
        *OutChannelMask = KSAUDIO_SPEAKER_MONO;
        return TRUE;
    case 2:
        *OutChannelMask = KSAUDIO_SPEAKER_STEREO;
        return TRUE;
    case 3:
        *OutChannelMask = KSAUDIO_SPEAKER_STEREO | SPEAKER_FRONT_CENTER;
        return TRUE;
    case 4:
        *OutChannelMask = KSAUDIO_SPEAKER_QUAD;
        return TRUE;
    case 5:
        *OutChannelMask = KSAUDIO_SPEAKER_QUAD | SPEAKER_FRONT_CENTER;
        return TRUE;
    case 6:
        *OutChannelMask = KSAUDIO_SPEAKER_5POINT1;
        return TRUE;
    case 7:
        *OutChannelMask = KSAUDIO_SPEAKER_5POINT1 | SPEAKER_BACK_CENTER;
        return TRUE;
    case 8:
        *OutChannelMask = KSAUDIO_SPEAKER_7POINT1;
        return TRUE;
    default:
        *OutChannelMask = 0;
        return FALSE;
    }
}

static BOOLEAN
VirtIoSndWaveRt_IsFormatSupportedEx(
    _In_opt_ const VIRTIOSND_WAVERT_MINIPORT* Miniport,
    _In_ const KSDATAFORMAT *DataFormat,
    _In_ BOOLEAN Capture,
    _Out_opt_ PVIRTIOSND_WAVERT_STREAM_FORMAT OutFormat)
{
    const KSDATAFORMAT_WAVEFORMATEXTENSIBLE *fmt;
    const WAVEFORMATEX *wfx;
    GUID subFormat;
    USHORT channels;
    USHORT bitsPerSample;
    USHORT validBitsPerSample;
    ULONG sampleRate;
    ULONG expectedBlockAlign;
    ULONG expectedAvgBytesPerSec;
    ULONG channelMask;
    ULONG i;
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    const VIRTIOSND_WAVERT_FORMAT_ENTRY* table;
    ULONG tableCount;
#endif

    if (OutFormat != NULL) {
        RtlZeroMemory(OutFormat, sizeof(*OutFormat));
    }

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

    channels = wfx->nChannels;
    bitsPerSample = wfx->wBitsPerSample;
    validBitsPerSample = wfx->wBitsPerSample;
    sampleRate = wfx->nSamplesPerSec;
    subFormat = KSDATAFORMAT_SUBTYPE_PCM;
    channelMask = 0;

    if (wfx->wFormatTag == WAVE_FORMAT_PCM) {
        subFormat = KSDATAFORMAT_SUBTYPE_PCM;
        if (!VirtIoSndWaveRtChannelMaskForChannels(channels, &channelMask)) {
            return FALSE;
        }
    } else if (wfx->wFormatTag == WAVE_FORMAT_IEEE_FLOAT) {
        subFormat = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        if (!VirtIoSndWaveRtChannelMaskForChannels(channels, &channelMask)) {
            return FALSE;
        }
    } else if (wfx->wFormatTag == WAVE_FORMAT_EXTENSIBLE) {
        if (DataFormat->FormatSize < sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE)) {
            return FALSE;
        }

        fmt = (const KSDATAFORMAT_WAVEFORMATEXTENSIBLE *)DataFormat;
        subFormat = fmt->WaveFormatExt.SubFormat;
        channelMask = fmt->WaveFormatExt.dwChannelMask;
        validBitsPerSample = fmt->WaveFormatExt.Samples.wValidBitsPerSample;
    } else {
        return FALSE;
    }

    if (channels == 0) {
        return FALSE;
    }

    expectedBlockAlign = (ULONG)channels * ((ULONG)bitsPerSample / 8u);
    expectedAvgBytesPerSec = sampleRate * expectedBlockAlign;

    if ((ULONG)wfx->nBlockAlign != expectedBlockAlign || wfx->nAvgBytesPerSec != expectedAvgBytesPerSec) {
        return FALSE;
    }

    if (validBitsPerSample == 0 || validBitsPerSample > bitsPerSample) {
        return FALSE;
    }
    if (wfx->wFormatTag != WAVE_FORMAT_EXTENSIBLE && validBitsPerSample != bitsPerSample) {
        return FALSE;
    }
    if (IsEqualGUID(&subFormat, &KSDATAFORMAT_SUBTYPE_IEEE_FLOAT) && validBitsPerSample != bitsPerSample) {
        return FALSE;
    }

#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    UNREFERENCED_PARAMETER(Miniport);
    UNREFERENCED_PARAMETER(i);

    /* Legacy I/O-port build is fixed-format (contract v1). */
    if (sampleRate != VIRTIOSND_SAMPLE_RATE ||
        bitsPerSample != VIRTIOSND_BITS_PER_SAMPLE ||
        channels != (Capture ? VIRTIOSND_CAPTURE_CHANNELS : VIRTIOSND_CHANNELS) ||
        channelMask != (Capture ? KSAUDIO_SPEAKER_MONO : KSAUDIO_SPEAKER_STEREO) ||
        !IsEqualGUID(&subFormat, &KSDATAFORMAT_SUBTYPE_PCM)) {
        return FALSE;
    }

    if (OutFormat != NULL) {
        OutFormat->Channels = channels;
        OutFormat->BitsPerSample = bitsPerSample;
        OutFormat->ValidBitsPerSample = validBitsPerSample;
        OutFormat->SampleRate = sampleRate;
        OutFormat->BlockAlign = expectedBlockAlign;
        OutFormat->AvgBytesPerSec = expectedAvgBytesPerSec;
        OutFormat->ChannelMask = channelMask;
        OutFormat->SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
        OutFormat->VirtioFormat = (UCHAR)VIRTIO_SND_PCM_FMT_S16;
        OutFormat->VirtioRate = (UCHAR)VIRTIO_SND_PCM_RATE_48000;
        VirtIoSndWaveRtFormatInitQuantum(OutFormat);
    }

    return TRUE;
#else
    table = NULL;
    tableCount = 0;
    if (Miniport != NULL) {
        if (Capture) {
            table = Miniport->CaptureFormats;
            tableCount = Miniport->CaptureFormatCount;
        } else {
            table = Miniport->RenderFormats;
            tableCount = Miniport->RenderFormatCount;
        }
    }

    if (table != NULL && tableCount != 0) {
        for (i = 0; i < tableCount; ++i) {
            const VIRTIOSND_WAVERT_STREAM_FORMAT* f = &table[i].Format;
            if (f->Channels == channels &&
                f->BitsPerSample == bitsPerSample &&
                f->ValidBitsPerSample == validBitsPerSample &&
                f->SampleRate == sampleRate &&
                f->ChannelMask == channelMask &&
                IsEqualGUID(&f->SubFormat, &subFormat)) {
                if (OutFormat != NULL) {
                    *OutFormat = *f;
                    VirtIoSndWaveRtFormatInitQuantum(OutFormat);
                }
                return TRUE;
            }
        }
        return FALSE;
    }

    /* Fallback to fixed contract v1 format. */
    if (sampleRate != VIRTIOSND_SAMPLE_RATE ||
        bitsPerSample != VIRTIOSND_BITS_PER_SAMPLE ||
        channels != (Capture ? VIRTIOSND_CAPTURE_CHANNELS : VIRTIOSND_CHANNELS) ||
        channelMask != (Capture ? KSAUDIO_SPEAKER_MONO : KSAUDIO_SPEAKER_STEREO) ||
        !IsEqualGUID(&subFormat, &KSDATAFORMAT_SUBTYPE_PCM)) {
        return FALSE;
    }

    if (OutFormat != NULL) {
        OutFormat->Channels = channels;
        OutFormat->BitsPerSample = bitsPerSample;
        OutFormat->ValidBitsPerSample = validBitsPerSample;
        OutFormat->SampleRate = sampleRate;
        OutFormat->BlockAlign = expectedBlockAlign;
        OutFormat->AvgBytesPerSec = expectedAvgBytesPerSec;
        OutFormat->ChannelMask = channelMask;
        OutFormat->SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
        OutFormat->VirtioFormat = (UCHAR)VIRTIO_SND_PCM_FMT_S16;
        OutFormat->VirtioRate = (UCHAR)VIRTIO_SND_PCM_RATE_48000;
        VirtIoSndWaveRtFormatInitQuantum(OutFormat);
    }

    return TRUE;
#endif
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
    ULONG blockAlign;

    linearFrames = Stream->FrozenLinearFrames;
    qpc = Stream->FrozenQpc;

    if (!Stream->Capture && Stream->State == KSSTATE_RUN && Stream->QpcFrequency != 0) {
        ULONGLONG deltaQpc;
        ULONGLONG elapsedFrames;

        qpc = NowQpc;

        deltaQpc = 0;
        if (NowQpc >= Stream->StartQpc) {
            deltaQpc = NowQpc - Stream->StartQpc;
        }

        elapsedFrames = (deltaQpc * (ULONGLONG)Stream->Format.SampleRate) / Stream->QpcFrequency;
        linearFrames = Stream->StartLinearFrames + elapsedFrames;
    }

    ringBytes = 0;
    if (Stream->BufferSize != 0) {
        blockAlign = Stream->Format.BlockAlign;
        if (blockAlign == 0) {
            blockAlign = Stream->Capture ? VIRTIOSND_CAPTURE_BLOCK_ALIGN : VIRTIOSND_BLOCK_ALIGN;
        }
        ringBytes = (ULONG)((linearFrames * (ULONGLONG)blockAlign) % (ULONGLONG)Stream->BufferSize);
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

static VOID
VirtIoSndWaveRtResetStopState(_Inout_ PVIRTIOSND_WAVERT_STREAM Stream)
{
    KIRQL oldIrql;
    PKEVENT oldEvent;

    oldEvent = NULL;
    KeAcquireSpinLock(&Stream->Lock, &oldIrql);
    Stream->State = KSSTATE_STOP;
    Stream->FrozenLinearFrames = 0;
    Stream->FrozenQpc = 0;
    Stream->StartQpc = 0;
    Stream->StartLinearFrames = 0;
    Stream->SubmittedLinearPositionBytes = 0;
    Stream->SubmittedRingPositionBytes = 0;
    Stream->PacketCount = 0;
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    Stream->LastTickTime100ns = 0;
#endif
    oldEvent = Stream->NotificationEvent;
    Stream->NotificationEvent = NULL;
    if (Stream->PositionRegister != NULL) {
        Stream->PositionRegister->PlayOffset = 0;
        Stream->PositionRegister->WriteOffset = 0;
    }
    VirtIoSndWaveRtWriteClockRegister(Stream, 0);
    KeReleaseSpinLock(&Stream->Lock, oldIrql);

    if (oldEvent != NULL) {
        ObDereferenceObject(oldEvent);
    }

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    /*
     * Ensure virtio-snd eventq period notifications don't retain a stale kernel
     * event pointer after the stream is stopped.
     */
    {
        VIRTIOSND_PORTCLS_DX dx;
        ULONG streamId;

        dx = (Stream->Miniport != NULL) ? Stream->Miniport->Dx : NULL;
        streamId = Stream->Capture ? VIRTIO_SND_CAPTURE_STREAM_ID : VIRTIO_SND_PLAYBACK_STREAM_ID;

        if (dx != NULL) {
            VirtIoSndEventqSetStreamNotificationEvent(dx, streamId, NULL);
        }
    }
#endif
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
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    /*
     * Reset tick coalescing when (re)starting the timer so a PAUSE->RUN transition
     * doesn't accidentally suppress the first DPC tick due to a stale timestamp
     * from the previous RUN segment.
     */
    Stream->LastTickTime100ns = 0;
#endif
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

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
static __forceinline VOID
VirtIoSndWaveRtSchedulePcmXrunRecovery(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx, _In_ ULONG StreamId)
{
    LONG bit;

    if (Dx == NULL) {
        return;
    }

    /*
     * Only the two contract streams are supported by this driver. Ignore any
     * out-of-range stream ids (malformed/spoofed events).
     */
    if (StreamId == VIRTIO_SND_PLAYBACK_STREAM_ID) {
        bit = 0x1;
    } else if (StreamId == VIRTIO_SND_CAPTURE_STREAM_ID) {
        bit = 0x2;
    } else {
        return;
    }

    VirtIoSndWaveRtInterlockedOr(&Dx->PcmXrunPendingMask, bit);

    /*
     * Coalesce XRUN recoveries into a single work item. If events are spammed we
     * still perform bounded control-plane work.
     */
    if (InterlockedCompareExchange(&Dx->PcmXrunWorkQueued, 1, 0) == 0) {
        /*
         * The work item is stored in the device extension; ensure the device
         * object stays alive until it runs (STOP/REMOVE can delete the device
         * object soon after StopHardware returns).
         */
        if (Dx->Self != NULL) {
            ObReferenceObject(Dx->Self);
        }
        ExInitializeWorkItem(&Dx->PcmXrunWorkItem, VirtIoSndWaveRtPcmXrunWorkItem, Dx);
        ExQueueWorkItem(&Dx->PcmXrunWorkItem, DelayedWorkQueue);
    }
}

static VOID
VirtIoSndWaveRtPcmXrunWorkItem(_In_ PVOID Context)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;
    LONG mask;
    BOOLEAN requeued;
    PDEVICE_OBJECT selfObject;

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Context;
    if (dx == NULL) {
        return;
    }

    requeued = FALSE;
    selfObject = dx->Self;

    for (;;) {
        mask = InterlockedExchange(&dx->PcmXrunPendingMask, 0);
        if (mask == 0) {
            break;
        }

        if (dx->Removed || !dx->Started || dx->Control.DmaCtx == NULL) {
            break;
        }

        /*
         * Best-effort XRUN recovery:
         *  - Re-issue PCM_START for the affected stream.
         *
         * virtiosnd_control intentionally sends START even when the local state
         * machine believes the stream is already Running, so this can recover
         * from device-side XRUN handling that implicitly stops a stream.
         */
        if ((mask & 0x1) != 0) {
            (VOID)VirtioSndCtrlStop(&dx->Control);
            (VOID)VirtioSndCtrlStart(&dx->Control);
        }
        if ((mask & 0x2) != 0) {
            (VOID)VirtioSndCtrlStop1(&dx->Control);
            (VOID)VirtioSndCtrlStart1(&dx->Control);
        }
    }

    InterlockedExchange(&dx->PcmXrunWorkQueued, 0);

    /*
     * If additional XRUNs arrived while we were running, re-queue once. This is
     * bounded by the single outstanding work item guarantee.
     */
    if (!dx->Removed && dx->Started && dx->Control.DmaCtx != NULL &&
        InterlockedCompareExchange(&dx->PcmXrunPendingMask, 0, 0) != 0) {
        if (InterlockedCompareExchange(&dx->PcmXrunWorkQueued, 1, 0) == 0) {
            ExInitializeWorkItem(&dx->PcmXrunWorkItem, VirtIoSndWaveRtPcmXrunWorkItem, dx);
            ExQueueWorkItem(&dx->PcmXrunWorkItem, DelayedWorkQueue);
            requeued = TRUE;
        }
    } else {
        /*
         * During teardown or when the control engine is unavailable, drop any
         * accumulated bits so we don't spin re-queuing a best-effort recovery.
         */
        (VOID)InterlockedExchange(&dx->PcmXrunPendingMask, 0);
    }

    if (!requeued && selfObject != NULL) {
        ObDereferenceObject(selfObject);
    }
}

static VOID
VirtIoSndWaveRtEventqCallback(_In_opt_ void* Context, _In_ ULONG Type, _In_ ULONG Data)
{
    PVIRTIOSND_WAVERT_MINIPORT miniport;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    PVIRTIOSND_WAVERT_STREAM stream;
    KIRQL miniportIrql;

    miniport = (PVIRTIOSND_WAVERT_MINIPORT)Context;
    if (miniport == NULL) {
        return;
    }

    dx = miniport->Dx;
    if (dx == NULL) {
        return;
    }

    if (dx->Removed || !dx->Started) {
        return;
    }

    /*
     * Only process PCM events (period-elapsed / XRUN). All other events are
     * handled within the virtio interrupt path (e.g. topology jack updates).
     */
    if (Type != VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED && Type != VIRTIO_SND_EVT_PCM_XRUN) {
        return;
    }

    stream = NULL;

    KeAcquireSpinLock(&miniport->Lock, &miniportIrql);

    if (Data == VIRTIO_SND_PLAYBACK_STREAM_ID) {
        stream = miniport->RenderStream;
    } else if (Data == VIRTIO_SND_CAPTURE_STREAM_ID) {
        stream = miniport->CaptureStream;
    }

    if (stream != NULL) {
        KIRQL streamIrql;

        KeAcquireSpinLock(&stream->Lock, &streamIrql);

        /*
         * Only act on PCM events while the pin is running.
         *
         * If the stream is stopping (STOP/PAUSE transition, teardown), avoid
         * queueing any additional work.
         */
        if (!stream->Stopping && stream->State == KSSTATE_RUN) {
            if (Type == VIRTIO_SND_EVT_PCM_XRUN) {
                static volatile LONG xrunLog;
                LONG n;

                n = InterlockedIncrement(&xrunLog);
                if (VirtIoSndWaveRtShouldLogRareCounter(n)) {
                    VIRTIOSND_TRACE_ERROR("wavert: eventq: PCM XRUN (stream=%lu count=%ld)\n", Data, n);
                }

                VirtIoSndWaveRtSchedulePcmXrunRecovery(dx, Data);

                if (!stream->Capture) {
                    /*
                     * Playback underrun recovery:
                     * realign the submission cursor to the current play position
                     * so the next DPC tick can refill a lead of audio cleanly.
                     */
                    LARGE_INTEGER nowQpc;
                    ULONGLONG linearFrames;
                    ULONG ringBytes;

                    nowQpc = KeQueryPerformanceCounter(NULL);
                    VirtIoSndWaveRtGetPositionSnapshot(stream, (ULONGLONG)nowQpc.QuadPart, &linearFrames, &ringBytes, NULL);

                    if (stream->PeriodBytes != 0 && stream->BufferSize != 0) {
                        ULONG blockAlign;
                        blockAlign = (stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign : VIRTIOSND_BLOCK_ALIGN;
                        stream->SubmittedLinearPositionBytes = linearFrames * (ULONGLONG)blockAlign;
                        stream->SubmittedRingPositionBytes = ringBytes;
                    }
                }
            }

             /*
              * Use the event as an additional wake-up mechanism for the stream
              * engine. Timer-based wakeups remain active as a fallback.
              *
              * Avoid repeatedly queueing the stream DPC while it is already
              * running (e.g. polling-only mode drains eventq from within the
              * stream DPC).
              */
            if (InterlockedCompareExchange(&stream->DpcActive, 0, 0) == 0) {
                (VOID)KeInsertQueueDpc(&stream->TimerDpc, NULL, NULL);
            }
        }

        KeReleaseSpinLock(&stream->Lock, streamIrql);
    }

    KeReleaseSpinLock(&miniport->Lock, miniportIrql);
}
#endif /* !defined(AERO_VIRTIO_SND_IOPORT_LEGACY) */

static VOID
VirtIoSndWaveRtWaitForRxIdle(_Inout_ PVIRTIOSND_WAVERT_STREAM Stream, _In_opt_ VIRTIOSND_PORTCLS_DX Dx)
{
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    UNREFERENCED_PARAMETER(Stream);
    UNREFERENCED_PARAMETER(Dx);
#else
    /*
     * Bounded teardown wait:
     *   - In the normal case we observe the in-flight RX completion quickly
     *     (period-sized, typically 10ms) via INTx or by polling the used ring.
     *   - In failure cases (device reset/misbehavior) the completion may never
     *     arrive. Avoid an unbounded hang in PortCls teardown paths by bounding
     *     the total wait time and forcing a best-effort device reset if needed.
     */
    const ULONG totalTimeoutMs = 2000u; /* 2 seconds total */
    const ULONGLONG timeout100ns = (ULONGLONG)totalTimeoutMs * 10u * 1000u; /* ms -> 100ns */
    LARGE_INTEGER timeout;
    ULONGLONG start100ns;
    ULONGLONG now100ns;
    ULONGLONG deadline100ns;
    LONG rxInFlight;
    BOOLEAN timedOut;
    BOOLEAN deviceGone;

    if (Stream == NULL) {
        return;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return;
    }

    rxInFlight = InterlockedCompareExchange(&Stream->RxInFlight, 0, 0);
    if (rxInFlight == 0) {
        /* Treat as idle even if the event is out-of-sync. */
        KeSetEvent(&Stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
        return;
    }

    /*
     * Wait for the in-flight RX request (if any) to complete.
     *
     * INTx interrupts should normally deliver RX completions, but if an interrupt
     * is delayed or lost, the completion can already be present in the used ring
     * without running the callback. Poll rxq while waiting to keep teardown
     * deterministic.
     */
    timeout.QuadPart = -(LONGLONG)(10u * 1000u * 10u); /* 10ms */

    start100ns = KeQueryInterruptTime();
    deadline100ns = start100ns + timeout100ns;

    timedOut = FALSE;
    deviceGone = FALSE;

    while (KeReadStateEvent(&Stream->RxIdleEvent) == 0) {
        if (Dx != NULL && (Dx->Removed || !Dx->Started)) {
            deviceGone = TRUE;
            break;
        }

        if (Dx != NULL) {
            (VOID)VirtIoSndHwDrainRxCompletions(Dx, NULL, NULL);
        }

        if (KeReadStateEvent(&Stream->RxIdleEvent) != 0) {
            break;
        }

        (VOID)KeWaitForSingleObject(&Stream->RxIdleEvent, Executive, KernelMode, FALSE, &timeout);

        now100ns = KeQueryInterruptTime();
        if (now100ns >= deadline100ns) {
            timedOut = TRUE;
            break;
        }
    }

    if (KeReadStateEvent(&Stream->RxIdleEvent) == 0) {
        rxInFlight = InterlockedCompareExchange(&Stream->RxInFlight, 0, 0);

        if (timedOut) {
            VIRTIOSND_TRACE_ERROR(
                "wavert: capture teardown: RX idle wait timed out after %lu ms (Started=%u Removed=%u RxInFlight=%ld)\n",
                totalTimeoutMs,
                (Dx != NULL && Dx->Started) ? 1u : 0u,
                (Dx != NULL && Dx->Removed) ? 1u : 0u,
                rxInFlight);

            /*
             * Fail-safe: reset/quiesce the device best-effort so DMA stops and no
             * further completions can reference this stream.
             */
            if (Dx != NULL) {
                VirtIoSndHwResetDeviceForTeardown(Dx);
            }
        } else if (deviceGone) {
            VIRTIOSND_TRACE_ERROR(
                "wavert: capture teardown: RX idle wait aborted (device stopped/removed) (Started=%u Removed=%u RxInFlight=%ld)\n",
                (Dx != NULL && Dx->Started) ? 1u : 0u,
                (Dx != NULL && Dx->Removed) ? 1u : 0u,
                rxInFlight);
        }

        /*
         * Allow teardown to continue. If we reached this point without observing
         * a completion, the device has either been reset/stopped or removed, so
         * it's safe to force the stream into an idle state.
         */
        InterlockedExchange(&Stream->RxInFlight, 0);
        KeSetEvent(&Stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
    }
#endif
}

static VOID
VirtIoSndWaveRtUpdateRegisters(
    _Inout_ PVIRTIOSND_WAVERT_STREAM Stream,
    _In_ ULONG RingPositionBytes,
    _In_ ULONGLONG Qpc
    )
{
    if (Stream->PositionRegister != NULL) {
        if (Stream->Capture) {
            Stream->PositionRegister->WriteOffset = RingPositionBytes;
        } else {
            Stream->PositionRegister->PlayOffset = RingPositionBytes;
        }
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
    PVOID bufferVa;
    UINT64 bufferDma;
    PMDL bufferMdl;
    PKEVENT notifyEvent;
    PVIRTIOSND_BACKEND backend;
    VIRTIOSND_PORTCLS_DX dx;
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
    ULONG startOffsetBytes;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    if (stream == NULL) {
        return;
    }

    InterlockedIncrement(&stream->DpcActive);

     /*
      * Optional polling-only bring-up path:
      *
      * If the device was started without any usable interrupt (neither MSI/MSI-X
      * nor legacy INTx), completions are not delivered by an ISR/DPC. Poll all
      * used rings once per WaveRT period tick so the control/event queues (and
      * any TX/RX completions) are reliably drained.
      *
      * IRQL: this routine runs at DISPATCH_LEVEL; VirtIoSndHwPollAllUsed is
      * DISPATCH_LEVEL-safe.
      */
     dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
     if (dx != NULL && dx->AllowPollingOnly && dx->Started && !dx->Removed &&
         !dx->MessageInterruptsActive && dx->Intx.InterruptObject == NULL) {
         VirtIoSndHwPollAllUsed(dx);
     }
#endif

    KeAcquireSpinLock(&stream->Lock, &oldIrql);

    if (stream->Stopping || stream->State != KSSTATE_RUN || stream->BufferDma.Va == NULL || stream->BufferSize == 0 || stream->PeriodBytes == 0 ||
        stream->PeriodBytes > stream->BufferSize) {
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        goto Exit;
    }

    dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    /*
     * Coalesce duplicate DPC wakeups:
     *  - WaveRT maintains a periodic timer for contract-v1 compatibility.
     *  - eventq PCM notifications can also queue this DPC (via the WaveRT eventq
     *    callback) to reduce reliance on polling/timers when eventq is active.
     *
     * Both sources can queue this DPC near the same period boundary. Gate the
     * DPC body so PacketCount and position updates advance at most once per
     * period, even if we observe back-to-back queued DPCs.
     */
    {
        const ULONGLONG nowTick100ns = KeQueryInterruptTime();
        ULONGLONG threshold100ns = stream->Period100ns;

        if (threshold100ns != 0) {
            threshold100ns = (threshold100ns * 3u) / 4u;
        }

        if (stream->LastTickTime100ns != 0 && threshold100ns != 0 && nowTick100ns >= stream->LastTickTime100ns &&
            (nowTick100ns - stream->LastTickTime100ns) < threshold100ns) {
            KeReleaseSpinLock(&stream->Lock, oldIrql);
            goto Exit;
        }

        stream->LastTickTime100ns = nowTick100ns;
    }
#endif

    periodBytes = stream->PeriodBytes;
    bufferSize = stream->BufferSize;
    bufferVa = stream->BufferDma.Va;
    bufferDma = VirtIoSndWaveRtBackendBase(&stream->BufferDma);
    bufferMdl = stream->BufferMdl;
    notifyEvent = stream->NotificationEvent;
    backend = stream->Miniport->Backend;
    dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;

    if (stream->Capture) {
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        PVOID buffer;

        UNREFERENCED_PARAMETER(dx);
        UNREFERENCED_PARAMETER(bufferMdl);

        if (notifyEvent != NULL) {
            ObReferenceObject(notifyEvent);
        }

        startOffsetBytes = stream->RxWriteOffsetBytes;
        buffer = stream->BufferDma.Va;
        KeReleaseSpinLock(&stream->Lock, oldIrql);

        qpcValue = (ULONGLONG)KeQueryPerformanceCounter(NULL).QuadPart;

        if (buffer != NULL && bufferSize != 0 && periodBytes != 0 && periodBytes <= bufferSize) {
            ULONG remaining;
            ULONG first;
            ULONG second;

            remaining = bufferSize - startOffsetBytes;
            first = (remaining < periodBytes) ? remaining : periodBytes;
            second = periodBytes - first;

            RtlZeroMemory((UCHAR*)buffer + startOffsetBytes, first);
            if (second != 0) {
                RtlZeroMemory(buffer, second);
            }
        }

        KeAcquireSpinLock(&stream->Lock, &oldIrql);

        if (bufferSize != 0 && periodBytes != 0 && periodBytes <= bufferSize) {
            ULONG blockAlign;
            ULONGLONG periodFrames;

            stream->RxWriteOffsetBytes = (startOffsetBytes + periodBytes) % bufferSize;

            blockAlign = (stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign :
                (stream->Capture ? VIRTIOSND_CAPTURE_BLOCK_ALIGN : VIRTIOSND_BLOCK_ALIGN);
            periodFrames = (blockAlign != 0) ? ((ULONGLONG)periodBytes / (ULONGLONG)blockAlign) : 0;
            stream->FrozenLinearFrames += periodFrames;
            stream->FrozenQpc = qpcValue;
            stream->PacketCount += 1;

            VirtIoSndWaveRtUpdateRegisters(stream, stream->RxWriteOffsetBytes, qpcValue);
        } else {
            stream->RxWriteOffsetBytes = 0;
        }

        KeReleaseSpinLock(&stream->Lock, oldIrql);

        KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);

        if (notifyEvent != NULL) {
            KeSetEvent(notifyEvent, IO_NO_INCREMENT, FALSE);
            ObDereferenceObject(notifyEvent);
        }

        goto Exit;
#else
        if (stream->Miniport == NULL || !stream->Miniport->UseVirtioBackend || dx == NULL || dx->Removed || !dx->Started) {
            /*
             * If the virtio transport is unavailable (e.g. ForceNullBackend bring-up,
             * START_DEVICE failure, or device removal), keep the WaveRT capture pin
             * progressing with deterministic silence so user-mode capture clients
             * don't stall.
             */
            startOffsetBytes = stream->RxWriteOffsetBytes;
            stream->RxPendingOffsetBytes = startOffsetBytes;
            KeReleaseSpinLock(&stream->Lock, oldIrql);

            VirtIoSndWaveRtRxCompletion(stream,
                                        STATUS_SUCCESS,
                                        VIRTIO_SND_S_OK,
                                        0,
                                        0,
                                        (UINT32)sizeof(VIRTIO_SND_PCM_STATUS),
                                        NULL);
            goto Exit;
        }

        if (bufferMdl == NULL) {
            KeReleaseSpinLock(&stream->Lock, oldIrql);
            goto Exit;
        }

        /*
         * Drain RX completions at the start of each tick.
         *
         * This keeps capture progressing even if rxq interrupts are delayed,
         * lost, or suppressed (e.g. because the device completes buffers
         * immediately and would otherwise interrupt-storm).
         *
         * Important: release the stream lock before draining so the RX completion
         * callback can safely take it to advance the write cursor.
         */
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        (VOID)VirtIoSndHwDrainRxCompletions(dx, NULL, NULL);

        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        if (stream->Stopping || stream->State != KSSTATE_RUN || stream->BufferDma.Va == NULL || stream->BufferSize == 0 ||
            stream->PeriodBytes == 0 || stream->PeriodBytes > stream->BufferSize) {
            KeReleaseSpinLock(&stream->Lock, oldIrql);
            goto Exit;
        }

        periodBytes = stream->PeriodBytes;
        bufferSize = stream->BufferSize;
        bufferMdl = stream->BufferMdl;

        if (dx == NULL || dx->Removed || !dx->Started || bufferMdl == NULL) {
            KeReleaseSpinLock(&stream->Lock, oldIrql);
            goto Exit;
        }

        if (InterlockedCompareExchange(&stream->RxInFlight, 0, 0) != 0) {
            KeReleaseSpinLock(&stream->Lock, oldIrql);
            goto Exit;
        }

        startOffsetBytes = stream->RxWriteOffsetBytes;
        stream->RxPendingOffsetBytes = startOffsetBytes;
        InterlockedExchange(&stream->RxInFlight, 1);
        KeResetEvent(&stream->RxIdleEvent);

        KeReleaseSpinLock(&stream->Lock, oldIrql);

        {
            virtio_sg_entry_t sg[VIRTIOSND_RX_MAX_PAYLOAD_SG];
            USHORT sgCount = 0;
            VIRTIOSND_RX_SEGMENT segs[VIRTIOSND_RX_MAX_PAYLOAD_SG];
            USHORT i;

            status = VirtIoSndSgBuildFromMdlRegionEx(bufferMdl,
                                                     bufferSize,
                                                     startOffsetBytes,
                                                     periodBytes,
                                                     TRUE,  /* Wrap */
                                                     TRUE,  /* DeviceWrites (RX) */
                                                     sg,
                                                     (USHORT)RTL_NUMBER_OF(sg),
                                                     &sgCount);
            if (!NT_SUCCESS(status)) {
                /*
                 * Keep capture progressing with deterministic silence. If we fail
                 * to build the SG list (e.g. because the MDL region would exceed
                 * the indirect descriptor limit), treat it like an IO_ERR period
                 * so user-mode capture clients don't stall.
                 */
                VirtIoSndWaveRtRxCompletion(stream,
                                            status,
                                            VIRTIO_SND_S_IO_ERR,
                                            0,
                                            0,
                                            (UINT32)sizeof(VIRTIO_SND_PCM_STATUS),
                                            NULL);
                goto Exit;
            }

            for (i = 0; i < sgCount; i++) {
                segs[i].addr = sg[i].addr;
                segs[i].len = sg[i].len;
            }

            status = VirtIoSndHwSubmitRxSg(dx, segs, sgCount, stream);
            if (!NT_SUCCESS(status)) {
                /*
                 * If the RX submission fails, keep the capture pin's timeline
                 * moving forward by completing the period as silence.
                 */
                VirtIoSndWaveRtRxCompletion(stream,
                                            status,
                                            VIRTIO_SND_S_IO_ERR,
                                            0,
                                            0,
                                            (UINT32)sizeof(VIRTIO_SND_PCM_STATUS),
                                            NULL);
                goto Exit;
            }
        }

        goto Exit;
#endif
    }

    if (notifyEvent != NULL) {
        ObReferenceObject(notifyEvent);
    }

    qpc = KeQueryPerformanceCounter(NULL);
    qpcValue = (ULONGLONG)qpc.QuadPart;

    VirtIoSndWaveRtGetPositionSnapshot(stream, qpcValue, &linearFrames, &playOffsetBytes, NULL);
    playLinearBytes = linearFrames * (ULONGLONG)((stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign : VIRTIOSND_BLOCK_ALIGN);

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
            NTSTATUS writeStatus;

            queuedBytes = submittedLinearBytes - playLinearBytes;
            if (queuedBytes >= leadBytes) {
                break;
            }

            writeStatus = STATUS_INVALID_DEVICE_STATE;

            if (backend->Ops != NULL && backend->Ops->WritePeriodSg != NULL && bufferMdl != NULL) {
                virtio_sg_entry_t sg[VIRTIOSND_TX_MAX_SEGMENTS];
                USHORT sgCount;
                VIRTIOSND_TX_SEGMENT segs[VIRTIOSND_TX_MAX_SEGMENTS];
                USHORT i;

                sgCount = 0;
                writeStatus = VirtIoSndSgBuildFromMdlRegion(
                    bufferMdl,
                    bufferSize,
                    submittedRingBytes,
                    periodBytes,
                    TRUE,
                    sg,
                    (USHORT)RTL_NUMBER_OF(sg),
                    &sgCount);
                if (NT_SUCCESS(writeStatus)) {
                    for (i = 0; i < sgCount; i++) {
                        segs[i].Address.QuadPart = (LONGLONG)sg[i].addr;
                        segs[i].Length = (ULONG)sg[i].len;
                    }

                    writeStatus = VirtIoSndBackend_WritePeriodSg(backend, segs, (ULONG)sgCount);
                }
            }

            if (!NT_SUCCESS(writeStatus) && backend->Ops != NULL && backend->Ops->WritePeriodCopy != NULL && bufferVa != NULL) {
                ULONG remaining;
                ULONG first;
                ULONG second;

                remaining = bufferSize - submittedRingBytes;
                first = (remaining < periodBytes) ? remaining : periodBytes;
                second = periodBytes - first;

                writeStatus = VirtIoSndBackend_WritePeriodCopy(
                    backend,
                    (const UCHAR*)bufferVa + submittedRingBytes,
                    first,
                    (second != 0) ? bufferVa : NULL,
                    second,
                    FALSE /* AllowSilenceFill */);
            }

            if (!NT_SUCCESS(writeStatus)) {
                ULONG remaining;
                ULONG first;
                ULONG second;

                remaining = bufferSize - submittedRingBytes;
                first = (remaining < periodBytes) ? remaining : periodBytes;
                second = periodBytes - first;

                writeStatus = VirtIoSndBackend_WritePeriod(
                    backend,
                    bufferDma + (UINT64)submittedRingBytes,
                    first,
                    (second != 0) ? bufferDma : 0,
                    second);
            }
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

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
static VOID
VirtIoSndWaveRtRxCompletion(
    _In_opt_ void* Cookie,
    _In_ NTSTATUS CompletionStatus,
    _In_ ULONG VirtioStatus,
    _In_ ULONG LatencyBytes,
    _In_ ULONG PayloadBytes,
    _In_ UINT32 UsedLen,
    _In_opt_ void* Context)
{
    PVIRTIOSND_WAVERT_STREAM stream;
    ULONGLONG qpcValue;
    KIRQL oldIrql;
    ULONG pendingOffset;
    ULONG bufferSize;
    ULONG periodBytes;
    PVOID buffer;
    PKEVENT notifyEvent;
    BOOLEAN streamRunning;
    BOOLEAN ok;

    UNREFERENCED_PARAMETER(LatencyBytes);
    UNREFERENCED_PARAMETER(UsedLen);
    UNREFERENCED_PARAMETER(Context);

    stream = (PVIRTIOSND_WAVERT_STREAM)Cookie;
    if (stream == NULL) {
        return;
    }

    ok = (NT_SUCCESS(CompletionStatus) && VirtioStatus == VIRTIO_SND_S_OK) ? TRUE : FALSE;

    /*
     * Ensure device-written PCM bytes are visible to the CPU before user-mode
     * reads from the cyclic buffer.
     */
    if (stream->BufferMdl != NULL) {
        VirtIoSndSgFlushIoBuffers(stream->BufferMdl, TRUE);
    }

    qpcValue = (ULONGLONG)KeQueryPerformanceCounter(NULL).QuadPart;

    notifyEvent = NULL;
    streamRunning = FALSE;

    KeAcquireSpinLock(&stream->Lock, &oldIrql);

    pendingOffset = stream->RxPendingOffsetBytes;
    bufferSize = stream->BufferSize;
    periodBytes = stream->PeriodBytes;
    buffer = stream->BufferDma.Va;

    if (stream->State == KSSTATE_RUN && !stream->Stopping && stream->NotificationEvent != NULL) {
        streamRunning = TRUE;
        notifyEvent = stream->NotificationEvent;
        ObReferenceObject(notifyEvent);
    }

    KeReleaseSpinLock(&stream->Lock, oldIrql);

    /*
     * Per contract, the device completes rxq buffers with IO_ERR when the
     * capture stream is not running. In that case (and for any other error),
     * treat the payload as invalid and return full-period silence.
     */
    if (!ok) {
        PayloadBytes = 0;
        if (streamRunning) {
            VIRTIOSND_TRACE_ERROR("wavert: capture rx completion error: nt=0x%08X virtio=%lu (%s)\n",
                                  (UINT)CompletionStatus,
                                  VirtioStatus,
                                  VirtioSndStatusToString(VirtioStatus));
        }
    }

    /*
     * If the device reports a short write, still treat it as a full period and
     * fill any missing tail bytes with silence.
     */
    if (buffer != NULL && bufferSize != 0 && periodBytes != 0 && periodBytes <= bufferSize) {
        ULONG written;
        ULONG remaining;

        written = PayloadBytes;
        if (written > periodBytes) {
            written = periodBytes;
        }

        remaining = periodBytes - written;
        if (remaining != 0) {
            ULONG tailOffset;
            ULONG tailRemaining;
            ULONG first;
            ULONG second;

            tailOffset = (pendingOffset + written) % bufferSize;
            tailRemaining = bufferSize - tailOffset;
            first = (tailRemaining < remaining) ? tailRemaining : remaining;
            second = remaining - first;

            RtlZeroMemory((UCHAR*)buffer + tailOffset, first);
            if (second != 0) {
                RtlZeroMemory(buffer, second);
            }
        }
    }

    KeAcquireSpinLock(&stream->Lock, &oldIrql);

    if (bufferSize != 0 && periodBytes != 0 && periodBytes <= bufferSize) {
        ULONG blockAlign;
        ULONGLONG periodFrames;

        stream->RxWriteOffsetBytes = (pendingOffset + periodBytes) % bufferSize;

        blockAlign = (stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign :
            (stream->Capture ? VIRTIOSND_CAPTURE_BLOCK_ALIGN : VIRTIOSND_BLOCK_ALIGN);
        periodFrames = (blockAlign != 0) ? ((ULONGLONG)periodBytes / (ULONGLONG)blockAlign) : 0;
        stream->FrozenLinearFrames += periodFrames;
        stream->FrozenQpc = qpcValue;
        stream->PacketCount += 1;

        VirtIoSndWaveRtUpdateRegisters(stream, stream->RxWriteOffsetBytes, qpcValue);
    } else {
        stream->RxWriteOffsetBytes = 0;
    }

    InterlockedExchange(&stream->RxInFlight, 0);
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);

    if (notifyEvent != NULL) {
        KeSetEvent(notifyEvent, IO_NO_INCREMENT, FALSE);
        ObDereferenceObject(notifyEvent);
    }
}
#endif

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
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        VirtIoSndWaveRtMiniport_FreeDynamicDescription(miniport);
        if (miniport->Dx != NULL) {
            PVIRTIOSND_DEVICE_EXTENSION dx = miniport->Dx;
            ULONG attempts;

            /*
             * Prevent any further eventq callbacks from referencing this miniport.
             *
             * Note: a callback may already be in-flight (loaded the function +
             * context under the EventqLock). Best-effort wait for it to drain
             * before freeing the miniport object.
             */
            VirtIoSndHwSetEventCallback(dx, NULL, NULL);

            if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
                LARGE_INTEGER delay;

                KeFlushQueuedDpcs();

                delay.QuadPart = -(LONGLONG)(1 * 10 * 1000); /* 1ms */
                for (attempts = 0; attempts < 200; ++attempts) {
                    if (InterlockedCompareExchange(&dx->EventqCallbackInFlight, 0, 0) == 0) {
                        break;
                    }
                    (void)KeDelayExecutionThread(KernelMode, FALSE, &delay);
                }
            }
        }
#endif
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
    VIRTIOSND_PORTCLS_DX dx;
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
    miniport->UseVirtioBackend = FALSE;

    if (!forceNullBackend && dx != NULL) {
        #if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        status = VirtIoSndBackendLegacy_Create(dx, &miniport->Backend);
        #else
        status = VirtIoSndBackendVirtio_Create(dx, &miniport->Backend);
        #endif
        if (NT_SUCCESS(status)) {
            miniport->UseVirtioBackend = TRUE;
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
            VIRTIOSND_TRACE("wavert: using legacy-ioport virtio backend\n");
#else
            VIRTIOSND_TRACE("wavert: using virtio backend\n");
            /*
             * Best-effort eventq hook: register for virtio-snd PCM events (XRUN /
             * period-elapsed) for robustness. Contract v1 does not emit events;
             * the miniport must remain correct without them.
             */
            if (dx != NULL && !dx->Removed) {
                VirtIoSndHwSetEventCallback(dx, VirtIoSndWaveRtEventqCallback, miniport);
            }
            status = VirtIoSndWaveRtMiniport_BuildDynamicDescription(miniport);
            if (!NT_SUCCESS(status)) {
                const VIRTIOSND_PCM_FORMAT renderSel = dx->Control.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID];
                const VIRTIOSND_PCM_FORMAT captureSel = dx->Control.SelectedFormat[VIRTIO_SND_CAPTURE_STREAM_ID];
                const BOOLEAN isContractV1 =
                    (renderSel.Channels == VIRTIOSND_CHANNELS &&
                     renderSel.Format == (UCHAR)VIRTIO_SND_PCM_FMT_S16 &&
                     renderSel.Rate == (UCHAR)VIRTIO_SND_PCM_RATE_48000 &&
                     captureSel.Channels == VIRTIOSND_CAPTURE_CHANNELS &&
                     captureSel.Format == (UCHAR)VIRTIO_SND_PCM_FMT_S16 &&
                     captureSel.Rate == (UCHAR)VIRTIO_SND_PCM_RATE_48000) ? TRUE : FALSE;

                VIRTIOSND_TRACE_ERROR(
                    "wavert: dynamic format exposure build failed: 0x%08X (contractV1=%u)\n",
                    (UINT)status,
                    (UINT)isContractV1);

                /*
                 * If the negotiated format is still the contract-v1 fixed format,
                 * we can fall back to the static descriptor. Otherwise, failing
                 * Init avoids advertising an incorrect mix format to Windows.
                 */
                if (!isContractV1) {
                    VirtIoSndHwSetEventCallback(dx, NULL, NULL);
                    VirtIoSndBackend_Destroy(miniport->Backend);
                    miniport->Backend = NULL;
                    miniport->UseVirtioBackend = FALSE;
                    return status;
                }
            }
#endif
            return STATUS_SUCCESS;
        }

        VIRTIOSND_TRACE_ERROR(
            "wavert: backend create failed: 0x%08X (falling back to null)\n",
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

static const KSDATARANGE_AUDIO g_VirtIoSndWaveRtDataRangePcmRender = {
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

static const KSDATARANGE_AUDIO g_VirtIoSndWaveRtDataRangePcmCapture = {
    {
        sizeof(KSDATARANGE_AUDIO),
        0,
        0,
        0,
        {STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO)},
        {STATICGUIDOF(KSDATAFORMAT_SUBTYPE_PCM)},
        {STATICGUIDOF(KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)},
    },
    VIRTIOSND_CAPTURE_CHANNELS,
    VIRTIOSND_BITS_PER_SAMPLE,
    VIRTIOSND_BITS_PER_SAMPLE,
    VIRTIOSND_SAMPLE_RATE,
    VIRTIOSND_SAMPLE_RATE,
};

static const PKSDATARANGE g_VirtIoSndWaveRtPinDataRangesRender[] = {
    (PKSDATARANGE)&g_VirtIoSndWaveRtDataRangePcmRender,
};

static const PKSDATARANGE g_VirtIoSndWaveRtPinDataRangesCapture[] = {
    (PKSDATARANGE)&g_VirtIoSndWaveRtDataRangePcmCapture,
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
    RTL_NUMBER_OF(g_VirtIoSndWaveRtPinDataRangesRender),
    (PKSDATARANGE *)g_VirtIoSndWaveRtPinDataRangesRender,
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

static const KSPIN_DESCRIPTOR g_VirtIoSndWaveRtKsPinDescriptorCapture = {
    1,
    (PKSPIN_INTERFACE)g_VirtIoSndWaveRtPinInterfaces,
    1,
    (PKSPIN_MEDIUM)g_VirtIoSndWaveRtPinMediums,
    RTL_NUMBER_OF(g_VirtIoSndWaveRtPinDataRangesCapture),
    (PKSDATARANGE *)g_VirtIoSndWaveRtPinDataRangesCapture,
    KSPIN_DATAFLOW_OUT,
    KSPIN_COMMUNICATION_SOURCE,
    &KSNODETYPE_MICROPHONE,
    &KSPINNAME_MICROPHONE,
};

static const KSPIN_DESCRIPTOR g_VirtIoSndWaveRtKsPinDescriptorBridgeCapture = {
    0,
    NULL,
    0,
    NULL,
    0,
    NULL,
    KSPIN_DATAFLOW_IN,
    KSPIN_COMMUNICATION_BRIDGE,
    &KSNODETYPE_WAVE_IN,
    &KSPINNAME_WAVE_IN,
};

static const PCPIN_DESCRIPTOR g_VirtIoSndWaveRtPins[] = {
    {1, 1, 0, NULL, g_VirtIoSndWaveRtKsPinDescriptorRender},
    {1, 1, 0, NULL, g_VirtIoSndWaveRtKsPinDescriptorBridge},
    {1, 1, 0, NULL, g_VirtIoSndWaveRtKsPinDescriptorCapture},
    {1, 1, 0, NULL, g_VirtIoSndWaveRtKsPinDescriptorBridgeCapture},
};

static const PCCONNECTION_DESCRIPTOR g_VirtIoSndWaveRtConnections[] = {
    {KSFILTER_NODE, VIRTIOSND_WAVE_PIN_RENDER, KSFILTER_NODE, VIRTIOSND_WAVE_PIN_BRIDGE},
    {KSFILTER_NODE, VIRTIOSND_WAVE_PIN_BRIDGE_CAPTURE, KSFILTER_NODE, VIRTIOSND_WAVE_PIN_CAPTURE},
};

static const GUID* g_VirtIoSndWaveRtCategories[] = {
    &KSCATEGORY_AUDIO,
    &KSCATEGORY_RENDER,
    &KSCATEGORY_CAPTURE,
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

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
static VOID
VirtIoSndWaveRtMiniport_FreeDynamicDescription(_Inout_ PVIRTIOSND_WAVERT_MINIPORT Miniport)
{
    if (Miniport == NULL) {
        return;
    }

    if (Miniport->FilterDescriptor != NULL) {
        ExFreePoolWithTag(Miniport->FilterDescriptor, VIRTIOSND_POOL_TAG);
        Miniport->FilterDescriptor = NULL;
    }
    if (Miniport->Pins != NULL) {
        ExFreePoolWithTag(Miniport->Pins, VIRTIOSND_POOL_TAG);
        Miniport->Pins = NULL;
    }

    if (Miniport->RenderDataRanges != NULL) {
        ExFreePoolWithTag(Miniport->RenderDataRanges, VIRTIOSND_POOL_TAG);
        Miniport->RenderDataRanges = NULL;
    }
    if (Miniport->RenderFormats != NULL) {
        ExFreePoolWithTag(Miniport->RenderFormats, VIRTIOSND_POOL_TAG);
        Miniport->RenderFormats = NULL;
    }
    Miniport->RenderFormatCount = 0;

    if (Miniport->CaptureDataRanges != NULL) {
        ExFreePoolWithTag(Miniport->CaptureDataRanges, VIRTIOSND_POOL_TAG);
        Miniport->CaptureDataRanges = NULL;
    }
    if (Miniport->CaptureFormats != NULL) {
        ExFreePoolWithTag(Miniport->CaptureFormats, VIRTIOSND_POOL_TAG);
        Miniport->CaptureFormats = NULL;
    }
    Miniport->CaptureFormatCount = 0;
}

static BOOLEAN
VirtIoSndWaveRtMapVirtioFormatToKs(
    _In_ UCHAR VirtioFormat,
    _Out_ USHORT* BitsPerSample,
    _Out_ USHORT* ValidBitsPerSample,
    _Out_ GUID* SubFormat)
{
    if (BitsPerSample == NULL || ValidBitsPerSample == NULL || SubFormat == NULL) {
        return FALSE;
    }

    switch (VirtioFormat) {
    case VIRTIO_SND_PCM_FMT_U8:
        *BitsPerSample = 8u;
        *ValidBitsPerSample = 8u;
        *SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_S16:
        *BitsPerSample = 16u;
        *ValidBitsPerSample = 16u;
        *SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_S24:
        /* 24-bit samples in a 32-bit container (see virtio_snd_pcm_fmt spec). */
        *BitsPerSample = 32u;
        *ValidBitsPerSample = 24u;
        *SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_S32:
        *BitsPerSample = 32u;
        *ValidBitsPerSample = 32u;
        *SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_FLOAT:
        *BitsPerSample = 32u;
        *ValidBitsPerSample = 32u;
        *SubFormat = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_FLOAT64:
        *BitsPerSample = 64u;
        *ValidBitsPerSample = 64u;
        *SubFormat = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        return TRUE;
    default:
        *BitsPerSample = 0;
        *ValidBitsPerSample = 0;
        *SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
        return FALSE;
    }
}

static NTSTATUS
VirtIoSndWaveRtBuildFormatTableFromCaps(
    _In_ const VIRTIOSND_PCM_CAPS* Caps,
    _In_ ULONG PreferredChannels,
    _Outptr_result_maybenull_ PVIRTIOSND_WAVERT_FORMAT_ENTRY* OutFormats,
    _Out_ PULONG OutFormatCount,
    _Outptr_result_maybenull_ PKSDATARANGE** OutDataRanges)
{
    static const UCHAR kVirtioFormats[] = {
        VIRTIO_SND_PCM_FMT_S16,
        VIRTIO_SND_PCM_FMT_S24,
        VIRTIO_SND_PCM_FMT_S32,
        VIRTIO_SND_PCM_FMT_FLOAT,
        VIRTIO_SND_PCM_FMT_FLOAT64,
        VIRTIO_SND_PCM_FMT_U8,
    };

    ULONG fmtIdx;
    ULONG rate;
    ULONG channels;
    ULONG channelsMin;
    ULONG channelsMax;
    ULONG count;
    PVIRTIOSND_WAVERT_FORMAT_ENTRY formats;
    PKSDATARANGE* ranges;
    ULONG out;

    if (OutFormats == NULL || OutFormatCount == NULL || OutDataRanges == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutFormats = NULL;
    *OutFormatCount = 0;
    *OutDataRanges = NULL;

    if (Caps == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    channelsMin = Caps->ChannelsMin;
    channelsMax = Caps->ChannelsMax;
    if (channelsMin == 0) {
        channelsMin = 1;
    }
    if (channelsMax < channelsMin) {
        return STATUS_NOT_SUPPORTED;
    }
    if (channelsMin > 8) {
        return STATUS_NOT_SUPPORTED;
    }
    if (channelsMax > 8) {
        channelsMax = 8;
    }

    /*
     * Preserve the Aero contract v1 fixed format as the first enumerated
     * supported format when available.
     *
     * In practice, Windows tends to treat the first enumerated format as the
     * "preferred" / default mix format for an endpoint.
     */
    if (PreferredChannels < channelsMin || PreferredChannels > channelsMax) {
        PreferredChannels = channelsMin;
    }

    count = 0;
    for (fmtIdx = 0; fmtIdx < RTL_NUMBER_OF(kVirtioFormats); ++fmtIdx) {
        const UCHAR vf = kVirtioFormats[fmtIdx];
        USHORT bits;
        USHORT validBits;
        GUID sub;

        if ((Caps->Formats & VIRTIO_SND_PCM_FMT_MASK(vf)) == 0) {
            continue;
        }

        bits = 0;
        validBits = 0;
        sub = KSDATAFORMAT_SUBTYPE_PCM;
        if (!VirtIoSndWaveRtMapVirtioFormatToKs(vf, &bits, &validBits, &sub) || bits == 0 || validBits == 0) {
            continue;
        }

        for (rate = 0; rate < 64u; ++rate) {
            ULONG rateHz;

            if ((Caps->Rates & VIRTIO_SND_PCM_RATE_MASK(rate)) == 0) {
                continue;
            }

            rateHz = 0;
            if (!VirtioSndPcmRateToHz((UCHAR)rate, &rateHz) || rateHz == 0) {
                continue;
            }

            for (channels = channelsMin; channels <= channelsMax; ++channels) {
                count++;
            }
        }
    }

    if (count == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    formats = (PVIRTIOSND_WAVERT_FORMAT_ENTRY)ExAllocatePoolWithTag(NonPagedPool, sizeof(*formats) * (SIZE_T)count, VIRTIOSND_POOL_TAG);
    if (formats == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    ranges = (PKSDATARANGE*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*ranges) * (SIZE_T)count, VIRTIOSND_POOL_TAG);
    if (ranges == NULL) {
        ExFreePoolWithTag(formats, VIRTIOSND_POOL_TAG);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(formats, sizeof(*formats) * (SIZE_T)count);
    RtlZeroMemory(ranges, sizeof(*ranges) * (SIZE_T)count);

    out = 0;
    for (fmtIdx = 0; fmtIdx < RTL_NUMBER_OF(kVirtioFormats); ++fmtIdx) {
        const UCHAR vf = kVirtioFormats[fmtIdx];
        USHORT bits;
        USHORT validBits;
        GUID sub;
        USHORT bytesPerSample;

        if ((Caps->Formats & VIRTIO_SND_PCM_FMT_MASK(vf)) == 0) {
            continue;
        }

        bits = 0;
        validBits = 0;
        sub = KSDATAFORMAT_SUBTYPE_PCM;
        if (!VirtIoSndWaveRtMapVirtioFormatToKs(vf, &bits, &validBits, &sub) || bits == 0 || validBits == 0) {
            continue;
        }

        bytesPerSample = 0;
        if (!VirtioSndPcmFormatToBytesPerSample(vf, &bytesPerSample) || bytesPerSample == 0) {
            continue;
        }

        /*
         * Emit the 48kHz contract-v1 rate first when present, followed by all
         * other supported rates in ascending virtio rate-code order.
         */
        for (rate = 0; rate < 2u; ++rate) {
            ULONG rateCode;
            ULONG rateHz;

            if (rate == 0) {
                rateCode = (ULONG)VIRTIO_SND_PCM_RATE_48000;
            } else {
                /* Start from zero and skip the preferred rate code. */
                rateCode = 0;
            }

            for (; rateCode < 64u; ++rateCode) {
                if (rate == 0 && rateCode != (ULONG)VIRTIO_SND_PCM_RATE_48000) {
                    continue;
                }
                if (rate != 0 && rateCode == (ULONG)VIRTIO_SND_PCM_RATE_48000) {
                    continue;
                }

                if ((Caps->Rates & VIRTIO_SND_PCM_RATE_MASK(rateCode)) == 0) {
                    continue;
                }

                rateHz = 0;
                if (!VirtioSndPcmRateToHz((UCHAR)rateCode, &rateHz) || rateHz == 0) {
                    continue;
                }

                /*
                 * Emit the preferred channel count first (if in range), followed
                 * by the remaining supported counts in ascending order.
                 */
                for (channels = 0; channels < 2u; ++channels) {
                    ULONG channelCount;
                    ULONG channelMask;
                    ULONG blockAlign;
                    ULONG avgBytesPerSec;

                    if (channels == 0) {
                        channelCount = PreferredChannels;
                    } else {
                        channelCount = channelsMin;
                    }

                    for (; channelCount <= channelsMax; ++channelCount) {
                        if (channels == 0 && channelCount != PreferredChannels) {
                            continue;
                        }
                        if (channels != 0 && channelCount == PreferredChannels) {
                            continue;
                        }

                        if (out >= count) {
                            /* Defensive: should never happen. */
                            break;
                        }

                        channelMask = 0;
                        if (!VirtIoSndWaveRtChannelMaskForChannels((USHORT)channelCount, &channelMask)) {
                            continue;
                        }

                        blockAlign = channelCount * (ULONG)bytesPerSample;
                        avgBytesPerSec = rateHz * blockAlign;

                        formats[out].Format.Channels = (USHORT)channelCount;
                        formats[out].Format.BitsPerSample = bits;
                        formats[out].Format.ValidBitsPerSample = validBits;
                        formats[out].Format.SampleRate = rateHz;
                        formats[out].Format.BlockAlign = blockAlign;
                        formats[out].Format.AvgBytesPerSec = avgBytesPerSec;
                        formats[out].Format.ChannelMask = channelMask;
                        formats[out].Format.SubFormat = sub;
                        formats[out].Format.VirtioFormat = vf;
                        formats[out].Format.VirtioRate = (UCHAR)rateCode;
                        VirtIoSndWaveRtFormatInitQuantum(&formats[out].Format);

                        formats[out].DataRange.DataRange.FormatSize = sizeof(KSDATARANGE_AUDIO);
                        formats[out].DataRange.DataRange.Flags = 0;
                        formats[out].DataRange.DataRange.SampleSize = 0;
                        formats[out].DataRange.DataRange.Reserved = 0;
                        formats[out].DataRange.DataRange.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
                        formats[out].DataRange.DataRange.SubFormat = sub;
                        formats[out].DataRange.DataRange.Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX;
                        formats[out].DataRange.MaximumChannels = channelCount;
                        formats[out].DataRange.MinimumBitsPerSample = validBits;
                        formats[out].DataRange.MaximumBitsPerSample = validBits;
                        formats[out].DataRange.MinimumSampleFrequency = rateHz;
                        formats[out].DataRange.MaximumSampleFrequency = rateHz;

                        ranges[out] = (PKSDATARANGE)&formats[out].DataRange;
                        out++;
                    }
                }

                if (rate == 0) {
                    /* Only emit the preferred rate once. */
                    break;
                }
            }

            if (rate == 0) {
                /* If the preferred rate is not supported, the loop will skip it anyway. */
                continue;
            }
        }
    }

    if (out == 0) {
        ExFreePoolWithTag(ranges, VIRTIOSND_POOL_TAG);
        ExFreePoolWithTag(formats, VIRTIOSND_POOL_TAG);
        return STATUS_NOT_SUPPORTED;
    }

    *OutFormats = formats;
    *OutFormatCount = out;
    *OutDataRanges = ranges;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndWaveRtMiniport_BuildDynamicDescription(_Inout_ PVIRTIOSND_WAVERT_MINIPORT Miniport)
{
    VIRTIOSND_PORTCLS_DX dx;
    LONG capsValid;
    VIRTIOSND_PCM_CAPS renderCaps;
    VIRTIOSND_PCM_CAPS captureCaps;
    VIRTIOSND_PCM_FORMAT renderSel;
    VIRTIOSND_PCM_FORMAT captureSel;
    NTSTATUS status;

    if (Miniport == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Miniport->FilterDescriptor != NULL) {
        return STATUS_SUCCESS;
    }

    dx = Miniport->Dx;
    if (dx == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    capsValid = InterlockedCompareExchange(&dx->Control.CapsValid, 0, 0);
    if (capsValid == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    /*
     * Expose exactly one format per pin: the deterministically negotiated format
     * selected during START_DEVICE (VIO-020).
     *
     * This avoids Windows picking a different format than the driver negotiated
     * with the device.
     */
    renderSel = dx->Control.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID];
    captureSel = dx->Control.SelectedFormat[VIRTIO_SND_CAPTURE_STREAM_ID];
    if (renderSel.Channels == 0 || captureSel.Channels == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    RtlZeroMemory(&renderCaps, sizeof(renderCaps));
    renderCaps.Formats = VIRTIO_SND_PCM_FMT_MASK(renderSel.Format);
    renderCaps.Rates = VIRTIO_SND_PCM_RATE_MASK(renderSel.Rate);
    renderCaps.ChannelsMin = renderSel.Channels;
    renderCaps.ChannelsMax = renderSel.Channels;

    RtlZeroMemory(&captureCaps, sizeof(captureCaps));
    captureCaps.Formats = VIRTIO_SND_PCM_FMT_MASK(captureSel.Format);
    captureCaps.Rates = VIRTIO_SND_PCM_RATE_MASK(captureSel.Rate);
    captureCaps.ChannelsMin = captureSel.Channels;
    captureCaps.ChannelsMax = captureSel.Channels;

    status = VirtIoSndWaveRtBuildFormatTableFromCaps(
        &renderCaps,
        (ULONG)renderSel.Channels,
        &Miniport->RenderFormats,
        &Miniport->RenderFormatCount,
        &Miniport->RenderDataRanges);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    status = VirtIoSndWaveRtBuildFormatTableFromCaps(
        &captureCaps,
        (ULONG)captureSel.Channels,
        &Miniport->CaptureFormats,
        &Miniport->CaptureFormatCount,
        &Miniport->CaptureDataRanges);
    if (!NT_SUCCESS(status)) {
        goto Fail;
    }

    Miniport->Pins = (PCPIN_DESCRIPTOR)ExAllocatePoolWithTag(
        NonPagedPool,
        sizeof(PCPIN_DESCRIPTOR) * RTL_NUMBER_OF(g_VirtIoSndWaveRtPins),
        VIRTIOSND_POOL_TAG);
    if (Miniport->Pins == NULL) {
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto Fail;
    }
    /*
     * Avoid C99 compound literals: the Windows 7 WDK toolchain uses an older C
     * compiler. Copy the baseline pin descriptors and then patch in the dynamic
     * data ranges.
     */
    RtlCopyMemory(Miniport->Pins, g_VirtIoSndWaveRtPins, sizeof(g_VirtIoSndWaveRtPins));

    Miniport->Pins[VIRTIOSND_WAVE_PIN_RENDER].KsPinDescriptor.DataRangesCount = Miniport->RenderFormatCount;
    Miniport->Pins[VIRTIOSND_WAVE_PIN_RENDER].KsPinDescriptor.DataRanges = Miniport->RenderDataRanges;

    Miniport->Pins[VIRTIOSND_WAVE_PIN_CAPTURE].KsPinDescriptor.DataRangesCount = Miniport->CaptureFormatCount;
    Miniport->Pins[VIRTIOSND_WAVE_PIN_CAPTURE].KsPinDescriptor.DataRanges = Miniport->CaptureDataRanges;

    Miniport->FilterDescriptor = (PCFILTER_DESCRIPTOR)ExAllocatePoolWithTag(NonPagedPool, sizeof(PCFILTER_DESCRIPTOR), VIRTIOSND_POOL_TAG);
    if (Miniport->FilterDescriptor == NULL) {
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto Fail;
    }

    *Miniport->FilterDescriptor = g_VirtIoSndWaveRtFilterDescriptor;
    Miniport->FilterDescriptor->Pins = Miniport->Pins;

    return STATUS_SUCCESS;

Fail:
    VirtIoSndWaveRtMiniport_FreeDynamicDescription(Miniport);
    return status;
}
#endif /* !defined(AERO_VIRTIO_SND_IOPORT_LEGACY) */

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_GetDescription(
    _In_ IMiniportWaveRT *This,
    _Outptr_ PPCFILTER_DESCRIPTOR *OutFilterDescriptor
    )
{
    PVIRTIOSND_WAVERT_MINIPORT miniport;

    if (OutFilterDescriptor == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    miniport = VirtIoSndWaveRtMiniportFromInterface(This);

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    if (miniport != NULL && miniport->UseVirtioBackend && miniport->Dx != NULL) {
        NTSTATUS buildStatus;
        const PVIRTIOSND_DEVICE_EXTENSION dx = miniport->Dx;

        buildStatus = VirtIoSndWaveRtMiniport_BuildDynamicDescription(miniport);
        if (!NT_SUCCESS(buildStatus) && miniport->FilterDescriptor == NULL && dx != NULL) {
            const VIRTIOSND_PCM_FORMAT renderSel = dx->Control.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID];
            const VIRTIOSND_PCM_FORMAT captureSel = dx->Control.SelectedFormat[VIRTIO_SND_CAPTURE_STREAM_ID];
            const BOOLEAN isContractV1 =
                (renderSel.Channels == VIRTIOSND_CHANNELS &&
                 renderSel.Format == (UCHAR)VIRTIO_SND_PCM_FMT_S16 &&
                 renderSel.Rate == (UCHAR)VIRTIO_SND_PCM_RATE_48000 &&
                 captureSel.Channels == VIRTIOSND_CAPTURE_CHANNELS &&
                 captureSel.Format == (UCHAR)VIRTIO_SND_PCM_FMT_S16 &&
                 captureSel.Rate == (UCHAR)VIRTIO_SND_PCM_RATE_48000) ? TRUE : FALSE;

            if (!isContractV1) {
                return buildStatus;
            }
        }
    }

    if (miniport != NULL && miniport->FilterDescriptor != NULL) {
        *OutFilterDescriptor = (PPCFILTER_DESCRIPTOR)miniport->FilterDescriptor;
        return STATUS_SUCCESS;
    }
#endif

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
    const VIRTIOSND_WAVERT_STREAM_FORMAT* chosen;
    VIRTIOSND_WAVERT_STREAM_FORMAT fixed;
    PVIRTIOSND_WAVERT_MINIPORT miniport;
    ULONG i;

    UNREFERENCED_PARAMETER(Irp);

    if (DataRange == NULL || ResultantFormatLength == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PinId != VIRTIOSND_WAVE_PIN_RENDER && PinId != VIRTIOSND_WAVE_PIN_CAPTURE) {
        return STATUS_NO_MATCH;
    }

    if (DataRange->FormatSize < sizeof(KSDATARANGE_AUDIO)) {
        return STATUS_NO_MATCH;
    }

    if (!IsEqualGUID(&DataRange->MajorFormat, &KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUID(&DataRange->Specifier, &KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) {
        return STATUS_NO_MATCH;
    }

    requested = (KSDATARANGE_AUDIO *)DataRange;

    chosen = NULL;
    miniport = VirtIoSndWaveRtMiniportFromInterface(This);

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    if (miniport != NULL && MatchingDataRange != NULL) {
        const VIRTIOSND_WAVERT_FORMAT_ENTRY* table;
        ULONG tableCount;

        table = NULL;
        tableCount = 0;
        if (PinId == VIRTIOSND_WAVE_PIN_CAPTURE) {
            table = miniport->CaptureFormats;
            tableCount = miniport->CaptureFormatCount;
        } else {
            table = miniport->RenderFormats;
            tableCount = miniport->RenderFormatCount;
        }

        for (i = 0; table != NULL && i < tableCount; ++i) {
            if (MatchingDataRange == (PKSDATARANGE)&table[i].DataRange) {
                chosen = &table[i].Format;
                break;
            }
        }
    }
#endif

    if (chosen == NULL) {
        /*
         * Fallback to fixed contract v1 formats.
         *
         * Note: Preserve the previous "range intersection" style check to avoid
         * returning a format for clearly incompatible requests.
         */
        RtlZeroMemory(&fixed, sizeof(fixed));
        fixed.Channels = (PinId == VIRTIOSND_WAVE_PIN_CAPTURE) ? VIRTIOSND_CAPTURE_CHANNELS : VIRTIOSND_CHANNELS;
        fixed.BitsPerSample = VIRTIOSND_BITS_PER_SAMPLE;
        fixed.ValidBitsPerSample = VIRTIOSND_BITS_PER_SAMPLE;
        fixed.SampleRate = VIRTIOSND_SAMPLE_RATE;
        fixed.BlockAlign = (PinId == VIRTIOSND_WAVE_PIN_CAPTURE) ? VIRTIOSND_CAPTURE_BLOCK_ALIGN : VIRTIOSND_BLOCK_ALIGN;
        fixed.AvgBytesPerSec =
            (PinId == VIRTIOSND_WAVE_PIN_CAPTURE) ? VIRTIOSND_CAPTURE_AVG_BYTES_PER_SEC : VIRTIOSND_AVG_BYTES_PER_SEC;
        fixed.ChannelMask = (PinId == VIRTIOSND_WAVE_PIN_CAPTURE) ? KSAUDIO_SPEAKER_MONO : KSAUDIO_SPEAKER_STEREO;
        fixed.SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
        fixed.VirtioFormat = (UCHAR)VIRTIO_SND_PCM_FMT_S16;
        fixed.VirtioRate = (UCHAR)VIRTIO_SND_PCM_RATE_48000;
        VirtIoSndWaveRtFormatInitQuantum(&fixed);

        if (requested->MaximumChannels < fixed.Channels ||
            requested->MinimumBitsPerSample > fixed.ValidBitsPerSample ||
            requested->MaximumBitsPerSample < fixed.ValidBitsPerSample ||
            requested->MinimumSampleFrequency > fixed.SampleRate ||
            requested->MaximumSampleFrequency < fixed.SampleRate) {
            return STATUS_NO_MATCH;
        }

        chosen = &fixed;
    }

    /*
     * Validate that the requested audio range can accept the chosen format.
     *
     * Even though PortCls selects MatchingDataRange from the pin's advertised
     * data ranges, KSDATARANGE_AUDIO only encodes a *maximum* channel count. If
     * the system chooses a data range with a larger channel count than the
     * request (e.g. because multiple ranges are compatible), ensure we return
     * NO_MATCH so PortCls can try another candidate.
     */
    if (requested->MaximumChannels < chosen->Channels ||
        requested->MinimumBitsPerSample > chosen->ValidBitsPerSample ||
        requested->MaximumBitsPerSample < chosen->ValidBitsPerSample ||
        requested->MinimumSampleFrequency > chosen->SampleRate ||
        requested->MaximumSampleFrequency < chosen->SampleRate) {
        return STATUS_NO_MATCH;
    }

    RtlZeroMemory(&format, sizeof(format));

    format.DataFormat.FormatSize = sizeof(format);
    format.DataFormat.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
    format.DataFormat.SubFormat = chosen->SubFormat;
    format.DataFormat.Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX;
    format.DataFormat.SampleSize = chosen->BlockAlign;

    format.WaveFormatExt.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    format.WaveFormatExt.Format.nChannels = chosen->Channels;
    format.WaveFormatExt.Format.nSamplesPerSec = chosen->SampleRate;
    format.WaveFormatExt.Format.nAvgBytesPerSec = chosen->AvgBytesPerSec;
    format.WaveFormatExt.Format.nBlockAlign = chosen->BlockAlign;
    format.WaveFormatExt.Format.wBitsPerSample = chosen->BitsPerSample;
    format.WaveFormatExt.Format.cbSize = sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX);

    format.WaveFormatExt.Samples.wValidBitsPerSample = chosen->ValidBitsPerSample;
    format.WaveFormatExt.dwChannelMask = chosen->ChannelMask;
    format.WaveFormatExt.SubFormat = chosen->SubFormat;

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
    VIRTIOSND_WAVERT_STREAM_FORMAT streamFormat;

    UNREFERENCED_PARAMETER(OuterUnknown);
    UNREFERENCED_PARAMETER(PoolType);
    UNREFERENCED_PARAMETER(PortStream);

    if (OutStream == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutStream = NULL;

    if ((Capture && Pin != VIRTIOSND_WAVE_PIN_CAPTURE) || (!Capture && Pin != VIRTIOSND_WAVE_PIN_RENDER)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!VirtIoSndWaveRt_IsFormatSupportedEx(miniport, DataFormat, Capture, &streamFormat)) {
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
    stream->Capture = Capture;
    stream->HwPrepared = FALSE;
    stream->Format = streamFormat;
    KeInitializeSpinLock(&stream->Lock);

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    /*
     * Persist the selected format into the device control engine so subsequent
     * SET_PARAMS uses the correct virtio-snd (channels/format/rate) tuple.
     */
    if (miniport != NULL && miniport->UseVirtioBackend && miniport->Dx != NULL && miniport->Dx->Started && !miniport->Dx->Removed) {
        (VOID)VirtioSndCtrlSelectFormat(
            &miniport->Dx->Control,
            Capture ? VIRTIO_SND_CAPTURE_STREAM_ID : VIRTIO_SND_PLAYBACK_STREAM_ID,
            (UCHAR)streamFormat.Channels,
            streamFormat.VirtioFormat,
            streamFormat.VirtioRate);
    }
#endif

    KeInitializeTimerEx(&stream->Timer, NotificationTimer);
    KeInitializeDpc(&stream->TimerDpc, VirtIoSndWaveRtDpcRoutine, stream);
    KeInitializeEvent(&stream->DpcIdleEvent, NotificationEvent, TRUE);

    /*
     * Default to ~10ms where possible, rounded up to the timer quantum (see
     * VIRTIOSND_WAVERT_STREAM_FORMAT::MsQuantum).
     */
    if (streamFormat.MsQuantum != 0 && streamFormat.BytesPerQuantum != 0) {
        ULONG desiredMs;
        ULONG quanta;

        desiredMs = 10u;
        quanta = (desiredMs + streamFormat.MsQuantum - 1u) / streamFormat.MsQuantum;
        if (quanta == 0) {
            quanta = 1u;
        }

        stream->PeriodBytes = quanta * streamFormat.BytesPerQuantum;
        stream->PeriodMs = quanta * streamFormat.MsQuantum;
    } else {
        /* Defensive fallback. */
        stream->PeriodMs = 10u;
        stream->PeriodBytes = (streamFormat.BlockAlign != 0) ? (streamFormat.BlockAlign * 10u) : 0;
    }
    stream->Period100ns = (ULONGLONG)stream->PeriodMs * 10u * 1000u;
    {
        LARGE_INTEGER qpcFreq;
        (VOID)KeQueryPerformanceCounter(&qpcFreq);
        stream->QpcFrequency = (ULONGLONG)qpcFreq.QuadPart;
    }

    stream->RxInFlight = 0;
    stream->RxPendingOffsetBytes = 0;
    stream->RxWriteOffsetBytes = 0;
    KeInitializeEvent(&stream->RxIdleEvent, NotificationEvent, TRUE);

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
    if (Capture) {
        if (miniport->CaptureStream != NULL) {
            KeReleaseSpinLock(&miniport->Lock, oldIrql);
            ExFreePoolWithTag(stream->PositionRegister, VIRTIOSND_POOL_TAG);
            ExFreePoolWithTag(stream->ClockRegister, VIRTIOSND_POOL_TAG);
            ExFreePoolWithTag(stream, VIRTIOSND_POOL_TAG);
            return STATUS_DEVICE_BUSY;
        }
        miniport->CaptureStream = stream;
    } else {
        if (miniport->RenderStream != NULL) {
            KeReleaseSpinLock(&miniport->Lock, oldIrql);
            ExFreePoolWithTag(stream->PositionRegister, VIRTIOSND_POOL_TAG);
            ExFreePoolWithTag(stream->ClockRegister, VIRTIOSND_POOL_TAG);
            ExFreePoolWithTag(stream, VIRTIOSND_POOL_TAG);
            return STATUS_DEVICE_BUSY;
        }
        miniport->RenderStream = stream;
    }
    KeReleaseSpinLock(&miniport->Lock, oldIrql);

    (VOID)VirtIoSndWaveRtMiniport_AddRef(This);

    if (StreamId != NULL) {
        *StreamId = Capture ? 1u : 0u;
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
        PKEVENT oldEvent;
        VIRTIOSND_PORTCLS_DX dx;

        dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;

        VirtIoSndWaveRtStopTimer(stream);

        oldEvent = NULL;
        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        state = stream->State;
        oldEvent = stream->NotificationEvent;
        stream->NotificationEvent = NULL;
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        if (oldEvent != NULL) {
            ObDereferenceObject(oldEvent);
        }

        dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        if (dx != NULL) {
            const ULONG streamId = stream->Capture ? VIRTIO_SND_CAPTURE_STREAM_ID : VIRTIO_SND_PLAYBACK_STREAM_ID;
            VirtIoSndEventqSetStreamNotificationEvent(dx, streamId, NULL);
        }
#endif

        if (stream->Capture) {
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
            UNREFERENCED_PARAMETER(dx);
            (VOID)InterlockedExchange(&stream->RxInFlight, 0);
            KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
#else
            if (stream->Miniport != NULL && stream->Miniport->UseVirtioBackend && dx != NULL && dx->Started && !dx->Removed) {
                if (state == KSSTATE_RUN) {
                    (VOID)VirtioSndCtrlStop1(&dx->Control);
                }

                VirtIoSndWaveRtWaitForRxIdle(stream, dx);

                /*
                 * If the RX idle wait timed out, VirtIoSndWaveRtWaitForRxIdle may
                 * have performed an emergency device reset and dropped Started.
                 * Avoid issuing further control commands in that case.
                 */
                if (state != KSSTATE_STOP && dx->Started && !dx->Removed) {
                    (VOID)VirtioSndCtrlRelease1(&dx->Control);
                }
            } else {
                (VOID)InterlockedExchange(&stream->RxInFlight, 0);
                KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
            }

            /*
             * Ensure a subsequent STOP_DEVICE/REMOVE_DEVICE teardown drain cannot
             * call back into a freed stream via the rxq completion callback.
             */
            if (dx != NULL && InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) != 0 && dx->Rx.Queue != NULL && dx->Rx.Requests != NULL) {
                VirtIoSndRxSetCompletionCallback(&dx->Rx, NULL, NULL);
            }
#endif
        } else if (stream->Miniport != NULL && stream->Miniport->Backend != NULL) {
            (VOID)VirtIoSndBackend_Stop(stream->Miniport->Backend);
            (VOID)VirtIoSndBackend_Release(stream->Miniport->Backend);
        }

        if (stream->Miniport != NULL) {
            KeAcquireSpinLock(&stream->Miniport->Lock, &oldIrql);
            if (stream->Capture) {
                if (stream->Miniport->CaptureStream == stream) {
                    stream->Miniport->CaptureStream = NULL;
                }
            } else {
                if (stream->Miniport->RenderStream == stream) {
                    stream->Miniport->RenderStream = NULL;
                }
            }
            KeReleaseSpinLock(&stream->Miniport->Lock, oldIrql);
        }

        if (stream->BufferMdl != NULL) {
            IoFreeMdl(stream->BufferMdl);
        }

#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        {
            VIRTIOSND_DMA_CONTEXT dummyCtx;
            RtlZeroMemory(&dummyCtx, sizeof(dummyCtx));
            VirtIoSndFreeCommonBuffer(&dummyCtx, &stream->BufferDma);
        }
#else
        VirtIoSndFreeCommonBuffer((stream->Miniport && stream->Miniport->Dx) ? &stream->Miniport->Dx->DmaCtx : NULL, &stream->BufferDma);
#endif

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
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    VIRTIOSND_WAVERT_STREAM_FORMAT fmt;
    VIRTIOSND_PORTCLS_DX dx;
    NTSTATUS status;

    if (!VirtIoSndWaveRt_IsFormatSupportedEx(stream->Miniport, DataFormat, stream->Capture, &fmt)) {
        return STATUS_NO_MATCH;
    }

    /*
     * Cache the selected format for subsequent buffer sizing / position
     * reporting.
     */
    {
        KIRQL oldIrql;
        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        stream->Format = fmt;
        KeReleaseSpinLock(&stream->Lock, oldIrql);
    }

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;
    status = STATUS_SUCCESS;
    if (stream->Miniport != NULL && stream->Miniport->UseVirtioBackend && dx != NULL && dx->Started && !dx->Removed) {
        status = VirtioSndCtrlSelectFormat(
            &dx->Control,
            stream->Capture ? VIRTIO_SND_CAPTURE_STREAM_ID : VIRTIO_SND_PLAYBACK_STREAM_ID,
            (UCHAR)fmt.Channels,
            fmt.VirtioFormat,
            fmt.VirtioRate);
        if (!NT_SUCCESS(status)) {
            return STATUS_NO_MATCH;
        }
    }
#else
    UNREFERENCED_PARAMETER(dx);
    UNREFERENCED_PARAMETER(status);
#endif

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
    VIRTIOSND_PORTCLS_DX dx;
    ULONG bufferSize;
    ULONG periodBytes;
    UINT64 bufferDma;
    PVOID bufferVa;
    PMDL bufferMdl;
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
    dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;
    status = STATUS_SUCCESS;
    bufferSize = 0;
    periodBytes = 0;
    bufferDma = 0;
    bufferVa = NULL;
    bufferMdl = NULL;

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    oldState = stream->State;

    if (oldState == State) {
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        return STATUS_SUCCESS;
    }

    if (stream->Capture) {
        KSSTATE current;

        /*
         * Timer transitions.
         *
         * Stop the timer first on any transition away from RUN so no DPC can race
         * with virtio-snd control operations (PASSIVE_LEVEL only).
         */
        KeReleaseSpinLock(&stream->Lock, oldIrql);

        if (oldState == KSSTATE_RUN && State != KSSTATE_RUN) {
            VirtIoSndWaveRtStopTimer(stream);
        } else if (State == KSSTATE_STOP || State == KSSTATE_ACQUIRE || State == KSSTATE_PAUSE) {
            VirtIoSndWaveRtStopTimer(stream);
        }

        current = oldState;

#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        UNREFERENCED_PARAMETER(dx);

        while (VirtIoSndWaveRtStateRank(current) < VirtIoSndWaveRtStateRank(State)) {
            if (current == KSSTATE_STOP) {
                BOOLEAN prepared;

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                bufferSize = stream->BufferSize;
                periodBytes = stream->PeriodBytes;
                stream->FrozenLinearFrames = 0;
                stream->FrozenQpc = 0;
                stream->StartQpc = 0;
                stream->StartLinearFrames = 0;
                stream->SubmittedLinearPositionBytes = 0;
                stream->SubmittedRingPositionBytes = 0;
                stream->RxPendingOffsetBytes = 0;
                stream->RxWriteOffsetBytes = 0;
                InterlockedExchange(&stream->RxInFlight, 0);
                KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
                stream->HwPrepared = FALSE;
                stream->PacketCount = 0;
                if (stream->PositionRegister != NULL) {
                    stream->PositionRegister->PlayOffset = 0;
                    stream->PositionRegister->WriteOffset = 0;
                }
                VirtIoSndWaveRtWriteClockRegister(stream, 0);
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                /*
                 * PortCls may transition the pin to ACQUIRE before the cyclic buffer
                 * is allocated. Mirror the modern path: enter ACQUIRE, but only
                 * consider the stream "prepared" once the buffer is valid.
                 */
                prepared = (bufferSize != 0 && periodBytes != 0 && periodBytes <= bufferSize) ? TRUE : FALSE;

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                stream->HwPrepared = prepared;
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
                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                bufferSize = stream->BufferSize;
                periodBytes = stream->PeriodBytes;
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                if (bufferSize == 0 || periodBytes == 0 || periodBytes > bufferSize) {
                    return STATUS_INVALID_DEVICE_STATE;
                }
                if (!stream->HwPrepared) {
                    KeAcquireSpinLock(&stream->Lock, &oldIrql);
                    stream->HwPrepared = TRUE;
                    KeReleaseSpinLock(&stream->Lock, oldIrql);
                }

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                stream->State = KSSTATE_RUN;
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                VirtIoSndWaveRtStartTimer(stream);
                (VOID)KeInsertQueueDpc(&stream->TimerDpc, NULL, NULL);

                current = KSSTATE_RUN;
                continue;
            }

            break;
        }

        while (VirtIoSndWaveRtStateRank(current) > VirtIoSndWaveRtStateRank(State)) {
            if (current == KSSTATE_RUN) {
                VirtIoSndWaveRtStopTimer(stream);

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                stream->State = KSSTATE_PAUSE;
                InterlockedExchange(&stream->RxInFlight, 0);
                KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                VirtIoSndWaveRtWaitForRxIdle(stream, dx);

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
                PKEVENT oldNotifyEvent;

                VirtIoSndWaveRtStopTimer(stream);

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                stream->HwPrepared = FALSE;
                oldNotifyEvent = stream->NotificationEvent;
                stream->NotificationEvent = NULL;
                stream->FrozenLinearFrames = 0;
                stream->FrozenQpc = 0;
                stream->StartQpc = 0;
                stream->StartLinearFrames = 0;
                stream->SubmittedLinearPositionBytes = 0;
                stream->SubmittedRingPositionBytes = 0;
                stream->RxPendingOffsetBytes = 0;
                stream->RxWriteOffsetBytes = 0;
                InterlockedExchange(&stream->RxInFlight, 0);
                KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
                stream->PacketCount = 0;
                if (stream->PositionRegister != NULL) {
                    stream->PositionRegister->PlayOffset = 0;
                    stream->PositionRegister->WriteOffset = 0;
                }
                VirtIoSndWaveRtWriteClockRegister(stream, 0);
                stream->State = KSSTATE_STOP;
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                if (oldNotifyEvent != NULL) {
                    ObDereferenceObject(oldNotifyEvent);
                }

                current = KSSTATE_STOP;
                continue;
            }

            break;
        }

        return STATUS_SUCCESS;
#else
        while (VirtIoSndWaveRtStateRank(current) < VirtIoSndWaveRtStateRank(State)) {
            if (current == KSSTATE_STOP) {
                BOOLEAN prepared;

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                bufferSize = stream->BufferSize;
                periodBytes = stream->PeriodBytes;
                stream->FrozenLinearFrames = 0;
                stream->FrozenQpc = 0;
                stream->StartQpc = 0;
                stream->StartLinearFrames = 0;
                stream->SubmittedLinearPositionBytes = 0;
                stream->SubmittedRingPositionBytes = 0;
                stream->RxPendingOffsetBytes = 0;
                stream->RxWriteOffsetBytes = 0;
                InterlockedExchange(&stream->RxInFlight, 0);
                KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
                stream->HwPrepared = FALSE;
                stream->PacketCount = 0;
                if (stream->PositionRegister != NULL) {
                    stream->PositionRegister->PlayOffset = 0;
                    stream->PositionRegister->WriteOffset = 0;
                }
                VirtIoSndWaveRtWriteClockRegister(stream, 0);
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                prepared = FALSE;

                /*
                 * PortCls may transition the pin to ACQUIRE before the cyclic buffer
                 * is allocated. Only attempt virtio-snd SET_PARAMS/PREPARE once we
                 * have a valid buffer/period size.
                 */
                if (bufferSize != 0 && periodBytes != 0 && periodBytes <= bufferSize) {
                    prepared = TRUE;

                    if (stream->Miniport != NULL && stream->Miniport->UseVirtioBackend && dx != NULL && dx->Started && !dx->Removed) {
                        prepared = FALSE;

                        {
                            ULONG frameBytes = (stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign : VIRTIOSND_CAPTURE_BLOCK_ALIGN;
                            if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) != 0 && dx->Rx.FrameBytes != frameBytes) {
                                VirtIoSndUninitRxEngine(dx);
                            }

                            if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) == 0) {
                                status = VirtIoSndInitRxEngineEx(dx, frameBytes, VIRTIOSND_QUEUE_SIZE_RXQ);
                            if (!NT_SUCCESS(status)) {
#ifdef STATUS_ALREADY_INITIALIZED
                                if (status != STATUS_ALREADY_INITIALIZED) {
                                    return status;
                                }
#else
                                return status;
#endif
                            }
                        }
                        }

                        VirtIoSndHwSetRxCompletionCallback(dx, VirtIoSndWaveRtRxCompletion, NULL);
                        VirtioSndQueueDisableInterrupts(&dx->Queues[VIRTIOSND_QUEUE_RX]);

                        status = VirtioSndCtrlSetParams1(&dx->Control, bufferSize, periodBytes);
                        if (!NT_SUCCESS(status)) {
                            return status;
                        }

                        status = VirtioSndCtrlPrepare1(&dx->Control);
                        if (!NT_SUCCESS(status)) {
                            (VOID)VirtioSndCtrlRelease1(&dx->Control);
                            return status;
                        }

                        prepared = TRUE;
                    }
                }

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                stream->HwPrepared = prepared;
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
                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                bufferSize = stream->BufferSize;
                periodBytes = stream->PeriodBytes;
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                if (bufferSize == 0 || periodBytes == 0 || periodBytes > bufferSize) {
                    return STATUS_INVALID_DEVICE_STATE;
                }
                if (!stream->HwPrepared) {
                    BOOLEAN prepared;

                    prepared = TRUE;

                    if (stream->Miniport != NULL && stream->Miniport->UseVirtioBackend && dx != NULL && dx->Started && !dx->Removed) {
                        VIRTIOSND_STREAM_STATE streamState;

                        prepared = FALSE;

                        {
                            ULONG frameBytes = (stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign : VIRTIOSND_CAPTURE_BLOCK_ALIGN;
                            if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) != 0 && dx->Rx.FrameBytes != frameBytes) {
                                VirtIoSndUninitRxEngine(dx);
                            }

                            if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) == 0) {
                                status = VirtIoSndInitRxEngineEx(dx, frameBytes, VIRTIOSND_QUEUE_SIZE_RXQ);
                            if (!NT_SUCCESS(status)) {
#ifdef STATUS_ALREADY_INITIALIZED
                                if (status != STATUS_ALREADY_INITIALIZED) {
                                    return status;
                                }
#else
                                return status;
#endif
                            }
                        }
                        }

                        VirtIoSndHwSetRxCompletionCallback(dx, VirtIoSndWaveRtRxCompletion, NULL);
                        VirtioSndQueueDisableInterrupts(&dx->Queues[VIRTIOSND_QUEUE_RX]);

                        /*
                         * If the cyclic buffer was allocated/reallocated while paused,
                         * (re)issue SET_PARAMS1/PREPARE1 so START1 can succeed.
                         */
                        streamState = dx->Control.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID];
                        if (streamState == VirtioSndStreamStateRunning) {
                            (VOID)VirtioSndCtrlStop1(&dx->Control);
                            streamState = dx->Control.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID];
                        }
                        if (streamState != VirtioSndStreamStateIdle && streamState != VirtioSndStreamStateParamsSet) {
                            (VOID)VirtioSndCtrlRelease1(&dx->Control);
                        }

                        status = VirtioSndCtrlSetParams1(&dx->Control, bufferSize, periodBytes);
                        if (!NT_SUCCESS(status)) {
                            return status;
                        }

                        status = VirtioSndCtrlPrepare1(&dx->Control);
                        if (!NT_SUCCESS(status)) {
                            (VOID)VirtioSndCtrlRelease1(&dx->Control);
                            return status;
                        }

                        prepared = TRUE;
                    }

                    KeAcquireSpinLock(&stream->Lock, &oldIrql);
                    stream->HwPrepared = prepared;
                    KeReleaseSpinLock(&stream->Lock, oldIrql);

                    if (!prepared) {
                        return STATUS_INVALID_DEVICE_STATE;
                    }
                }

                if (stream->Miniport != NULL && stream->Miniport->UseVirtioBackend && dx != NULL && dx->Started && !dx->Removed) {
                    status = VirtioSndCtrlStart1(&dx->Control);
                    if (!NT_SUCCESS(status)) {
                        return status;
                    }
                }

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                stream->State = KSSTATE_RUN;
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                VirtIoSndWaveRtStartTimer(stream);
                (VOID)KeInsertQueueDpc(&stream->TimerDpc, NULL, NULL);

                current = KSSTATE_RUN;
                continue;
            }

            break;
        }

        while (VirtIoSndWaveRtStateRank(current) > VirtIoSndWaveRtStateRank(State)) {
            if (current == KSSTATE_RUN) {
                VirtIoSndWaveRtStopTimer(stream);

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                stream->State = KSSTATE_PAUSE;
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                if (stream->Miniport != NULL && stream->Miniport->UseVirtioBackend && dx != NULL && dx->Started && !dx->Removed) {
                    (VOID)VirtioSndCtrlStop1(&dx->Control);
                } else {
                    (VOID)InterlockedExchange(&stream->RxInFlight, 0);
                    KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
                }

                VirtIoSndWaveRtWaitForRxIdle(stream, dx);

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
                PKEVENT oldNotifyEvent;

                VirtIoSndWaveRtStopTimer(stream);

                if (stream->Miniport != NULL && stream->Miniport->UseVirtioBackend && dx != NULL && dx->Started && !dx->Removed) {
                    (VOID)VirtioSndCtrlRelease1(&dx->Control);
                }

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                stream->HwPrepared = FALSE;
                oldNotifyEvent = stream->NotificationEvent;
                stream->NotificationEvent = NULL;
                stream->FrozenLinearFrames = 0;
                stream->FrozenQpc = 0;
                stream->StartQpc = 0;
                stream->StartLinearFrames = 0;
                stream->SubmittedLinearPositionBytes = 0;
                stream->SubmittedRingPositionBytes = 0;
                stream->RxPendingOffsetBytes = 0;
                stream->RxWriteOffsetBytes = 0;
                InterlockedExchange(&stream->RxInFlight, 0);
                KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
                stream->PacketCount = 0;
                if (stream->PositionRegister != NULL) {
                    stream->PositionRegister->PlayOffset = 0;
                    stream->PositionRegister->WriteOffset = 0;
                }
                VirtIoSndWaveRtWriteClockRegister(stream, 0);
                stream->State = KSSTATE_STOP;
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                if (oldNotifyEvent != NULL) {
                    ObDereferenceObject(oldNotifyEvent);
                }

                current = KSSTATE_STOP;
                continue;
            }

            break;
        }

        return STATUS_SUCCESS;
#endif
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
            elapsedFrames = (deltaQpc * (ULONGLONG)stream->Format.SampleRate) / stream->QpcFrequency;
        }

        stream->FrozenLinearFrames = stream->StartLinearFrames + elapsedFrames;
        stream->FrozenQpc = nowQpcValue;

        ringBytes = 0;
        if (stream->BufferSize != 0) {
            ULONG blockAlign = (stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign : VIRTIOSND_BLOCK_ALIGN;
            ringBytes = (ULONG)((stream->FrozenLinearFrames * (ULONGLONG)blockAlign) % (ULONGLONG)stream->BufferSize);
        }
        VirtIoSndWaveRtUpdateRegisters(stream, ringBytes, nowQpcValue);

        /*
         * Apply the non-RUN state immediately so:
         *  - QPC position reporting freezes (GetPositionSnapshot uses Frozen*).
         *  - The periodic DPC exits quickly even if a timer tick races with this transition.
         *
         * Backend STOP/RELEASE operations are still issued below (outside the spinlock).
         */
        stream->State = State;
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
    bufferDma = VirtIoSndWaveRtBackendBase(&stream->BufferDma);
    bufferVa = stream->BufferDma.Va;
    bufferMdl = stream->BufferMdl;
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
        if (State == KSSTATE_STOP) {
            VirtIoSndWaveRtResetStopState(stream);
        }
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
            ULONG blockAlign = (stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign : VIRTIOSND_BLOCK_ALIGN;
            startOffsetBytes = (ULONG)((startLinearFrames * (ULONGLONG)blockAlign) % (ULONGLONG)bufferSize);
        }

        VirtIoSndWaveRtUpdateRegisters(stream, startOffsetBytes, nowQpcValue);

        stream->SubmittedLinearPositionBytes =
            startLinearFrames * (ULONGLONG)((stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign : VIRTIOSND_BLOCK_ALIGN);
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
                NTSTATUS writeStatus;

                queuedBytes = submittedLinearBytes - playLinearBytes;
                if (queuedBytes >= leadBytes) {
                    break;
                }

                writeStatus = STATUS_INVALID_DEVICE_STATE;

                if (backend->Ops != NULL && backend->Ops->WritePeriodSg != NULL && bufferMdl != NULL) {
                    virtio_sg_entry_t sg[VIRTIOSND_TX_MAX_SEGMENTS];
                    USHORT sgCount;
                    VIRTIOSND_TX_SEGMENT segs[VIRTIOSND_TX_MAX_SEGMENTS];
                    USHORT i;

                    sgCount = 0;
                    writeStatus = VirtIoSndSgBuildFromMdlRegion(
                        bufferMdl,
                        bufferSize,
                        submittedRingBytes,
                        periodBytes,
                        TRUE,
                        sg,
                        (USHORT)RTL_NUMBER_OF(sg),
                        &sgCount);
                    if (NT_SUCCESS(writeStatus)) {
                        for (i = 0; i < sgCount; i++) {
                            segs[i].Address.QuadPart = (LONGLONG)sg[i].addr;
                            segs[i].Length = (ULONG)sg[i].len;
                        }

                        writeStatus = VirtIoSndBackend_WritePeriodSg(backend, segs, (ULONG)sgCount);
                    }
                }

                if (!NT_SUCCESS(writeStatus) && backend->Ops != NULL && backend->Ops->WritePeriodCopy != NULL && bufferVa != NULL) {
                    ULONG remaining;
                    ULONG first;
                    ULONG second;

                    remaining = bufferSize - submittedRingBytes;
                    first = (remaining < periodBytes) ? remaining : periodBytes;
                    second = periodBytes - first;

                    writeStatus = VirtIoSndBackend_WritePeriodCopy(
                        backend,
                        (const UCHAR*)bufferVa + submittedRingBytes,
                        first,
                        (second != 0) ? bufferVa : NULL,
                        second,
                        FALSE /* AllowSilenceFill */);
                }

                if (!NT_SUCCESS(writeStatus)) {
                    ULONG remaining;
                    ULONG first;
                    ULONG second;

                    remaining = bufferSize - submittedRingBytes;
                    first = (remaining < periodBytes) ? remaining : periodBytes;
                    second = periodBytes - first;

                    writeStatus = VirtIoSndBackend_WritePeriod(
                        backend,
                        bufferDma + (UINT64)submittedRingBytes,
                        first,
                        (second != 0) ? bufferDma : 0,
                        second);
                }
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
        VirtIoSndWaveRtResetStopState(stream);
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
    *Position = linearFrames * (ULONGLONG)((stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign :
        (stream->Capture ? VIRTIOSND_CAPTURE_BLOCK_ALIGN : VIRTIOSND_BLOCK_ALIGN));
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
    ULONG64 play;
    ULONG64 write;
    ULONG64 diff;
    KIRQL oldIrql;
    ULONG bufferBytes;
    ULONG ringBytes;

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
    VirtIoSndWaveRtGetPositionSnapshot(stream, qpcValue, &linearFrames, &ringBytes, &qpcForPosition);
    VirtIoSndWaveRtUpdateRegisters(stream, ringBytes, qpcForPosition);
    bufferBytes = stream->BufferSize;
    if (bufferBytes == 0) {
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        *PaddingFrames = 0;
        return STATUS_SUCCESS;
    }
    play = stream->PositionRegister->PlayOffset % bufferBytes;
    write = stream->PositionRegister->WriteOffset % bufferBytes;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (write >= play) {
        diff = write - play;
    } else {
        diff = (ULONG64)bufferBytes - play + write;
    }

    *PaddingFrames = (ULONG)(diff / (ULONG64)((stream->Format.BlockAlign != 0) ? stream->Format.BlockAlign :
        (stream->Capture ? VIRTIOSND_CAPTURE_BLOCK_ALIGN : VIRTIOSND_BLOCK_ALIGN)));
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

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    {
        VIRTIOSND_PORTCLS_DX dx;
        ULONG streamId;

        dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;
        streamId = stream->Capture ? VIRTIO_SND_CAPTURE_STREAM_ID : VIRTIO_SND_PLAYBACK_STREAM_ID;

        if (dx != NULL) {
            VirtIoSndEventqSetStreamNotificationEvent(dx, streamId, NotificationEvent);
        }
    }
#endif
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
    ULONG bytesPerQuantum;
    ULONG msQuantum;
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    VIRTIOSND_DMA_CONTEXT dummyCtx;
#endif
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

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    /*
     * Before reallocating the cyclic capture buffer/MDL, ensure there is no
     * in-flight RX period that could DMA-write into freed memory.
     */
    if (stream->Capture) {
        VIRTIOSND_PORTCLS_DX dx;
        dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;
        VirtIoSndWaveRtWaitForRxIdle(stream, dx);
    }
#endif

    bytesPerQuantum = stream->Format.BytesPerQuantum;
    msQuantum = stream->Format.MsQuantum;
    if (bytesPerQuantum == 0 || msQuantum == 0) {
        VIRTIOSND_WAVERT_STREAM_FORMAT tmp;
        tmp = stream->Format;
        VirtIoSndWaveRtFormatInitQuantum(&tmp);
        bytesPerQuantum = tmp.BytesPerQuantum;
        msQuantum = tmp.MsQuantum;
    }
    if (bytesPerQuantum == 0 || msQuantum == 0) {
        return STATUS_INVALID_DEVICE_STATE;
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

    /*
     * Ensure the buffer is large enough for at least one timer quantum per
     * notification.
     */
    if (bytesPerQuantum > MAXULONG / notifications) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    if (size < bytesPerQuantum * notifications) {
        size = bytesPerQuantum * notifications;
    }

    /*
     * Cap the cyclic DMA buffer allocation so user-controlled buffering requests
     * cannot trigger arbitrarily large nonpaged common-buffer allocations.
     */
    if (size > VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES) {
        size = VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES;
    }

    /*
     * Compute a period size aligned to the integer-millisecond timer quantum.
     * Ensure the
     * period payload never exceeds the contract maximum (256 KiB) by increasing
     * the notification count (up to the existing 256 cap).
     */
    for (;;) {
        periodBytes = (size + notifications - 1) / notifications;
        periodBytes = (periodBytes + (bytesPerQuantum - 1)) / bytesPerQuantum;
        periodBytes *= bytesPerQuantum;
        if (periodBytes < bytesPerQuantum) {
            periodBytes = bytesPerQuantum;
        }

        if (periodBytes <= VIRTIOSND_MAX_PCM_PAYLOAD_BYTES) {
            break;
        }

        if (notifications >= 256) {
            return STATUS_INVALID_BUFFER_SIZE;
        }
        notifications++;

        /*
         * If the caller requested an extremely small buffer but we increased
         * notifications, keep the minimum sizing invariant.
         */
        if (bytesPerQuantum > MAXULONG / notifications) {
            return STATUS_INVALID_BUFFER_SIZE;
        }
        if (size < bytesPerQuantum * notifications) {
            size = bytesPerQuantum * notifications;
        }
        if (size > VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES) {
            size = VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES;
        }
    }

    size = periodBytes * notifications;
    if ((size / notifications) != periodBytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    if (size > VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES) {
        ULONG quantum;
        ULONG maxSize;

        maxSize = VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES;

        if (bytesPerQuantum > MAXULONG / notifications) {
            return STATUS_INVALID_BUFFER_SIZE;
        }
        quantum = bytesPerQuantum * notifications;
        if (quantum == 0 || maxSize < quantum) {
            return STATUS_INVALID_BUFFER_SIZE;
        }

        /*
         * Round down to a representable (periodBytes * notifications) size that
         * fits within the allocation cap.
         */
        size = (maxSize / quantum) * quantum;
        if (size < quantum) {
            size = quantum;
        }
        periodBytes = size / notifications;
    }

#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    RtlZeroMemory(&dummyCtx, sizeof(dummyCtx));
    dmaCtx = &dummyCtx;
#else
    dmaCtx = (stream->Miniport && stream->Miniport->Dx) ? &stream->Miniport->Dx->DmaCtx : NULL;
#endif
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
    stream->PeriodMs = (periodBytes / bytesPerQuantum) * msQuantum;
    stream->Period100ns = (ULONGLONG)stream->PeriodMs * 10u * 1000u;

    stream->FrozenLinearFrames = 0;
    stream->FrozenQpc = 0;
    stream->StartQpc = 0;
    stream->StartLinearFrames = 0;
    stream->SubmittedLinearPositionBytes = 0;
    stream->SubmittedRingPositionBytes = 0;
    stream->RxPendingOffsetBytes = 0;
    stream->RxWriteOffsetBytes = 0;
    InterlockedExchange(&stream->RxInFlight, 0);
    KeSetEvent(&stream->RxIdleEvent, IO_NO_INCREMENT, FALSE);
    stream->HwPrepared = FALSE;
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

    if (!stream->Capture && stream->Miniport != NULL && stream->Miniport->Backend != NULL) {
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
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    VIRTIOSND_DMA_CONTEXT dummyCtx;
#endif
    PMDL oldMdl;
    VIRTIOSND_DMA_BUFFER oldDma;
    KIRQL oldIrql;

    VirtIoSndWaveRtStopTimer(stream);

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    /*
     * Ensure capture RX is idle before freeing the cyclic buffer so no device
     * DMA can target freed memory.
     */
    if (stream->Capture) {
        VIRTIOSND_PORTCLS_DX dx;
        dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;
        VirtIoSndWaveRtWaitForRxIdle(stream, dx);
    }
#endif

#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    RtlZeroMemory(&dummyCtx, sizeof(dummyCtx));
    dmaCtx = &dummyCtx;
#else
    dmaCtx = (stream->Miniport && stream->Miniport->Dx) ? &stream->Miniport->Dx->DmaCtx : NULL;
#endif

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
