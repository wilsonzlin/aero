/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_control_proto.h"
#include "virtiosnd_limits.h"

/*
 * Deterministic virtio-snd PCM negotiation.
 *
 * Keep the selection logic in this host-testable module so it can be exercised
 * by unit tests without bringing up the full WDM control engine.
 */

static __forceinline BOOLEAN VirtioSndCtrlIsSupportedVirtioPcmFormat(_In_ UCHAR Format)
{
    /*
     * Supported subset for the Win7 WaveRT miniport:
     *  - PCM:   U8, S16, S24, S32
     *  - Float: 32-bit, 64-bit
     */
    switch (Format) {
    case VIRTIO_SND_PCM_FMT_U8:
    case VIRTIO_SND_PCM_FMT_S16:
    case VIRTIO_SND_PCM_FMT_S24:
    case VIRTIO_SND_PCM_FMT_S32:
    case VIRTIO_SND_PCM_FMT_FLOAT:
    case VIRTIO_SND_PCM_FMT_FLOAT64:
        return TRUE;
    default:
        return FALSE;
    }
}

static __forceinline BOOLEAN VirtioSndCtrlIsValidStreamId(_In_ ULONG StreamId)
{
    return (StreamId == VIRTIO_SND_PLAYBACK_STREAM_ID || StreamId == VIRTIO_SND_CAPTURE_STREAM_ID) ? TRUE : FALSE;
}

static __forceinline UCHAR VirtioSndCtrlFixedChannelsForStream(_In_ ULONG StreamId)
{
    return (StreamId == VIRTIO_SND_CAPTURE_STREAM_ID) ? 1 : 2;
}

_Use_decl_annotations_
NTSTATUS VirtioSndCtrlSelectPcmConfig(const VIRTIO_SND_PCM_INFO* Info, ULONG StreamId, VIRTIOSND_PCM_CONFIG* OutConfig)
{
    static const UCHAR kFormatPriority[] = {
        (UCHAR)VIRTIO_SND_PCM_FMT_S16,
        (UCHAR)VIRTIO_SND_PCM_FMT_S24,
        (UCHAR)VIRTIO_SND_PCM_FMT_S32,
        (UCHAR)VIRTIO_SND_PCM_FMT_FLOAT,
        (UCHAR)VIRTIO_SND_PCM_FMT_FLOAT64,
        (UCHAR)VIRTIO_SND_PCM_FMT_U8,
    };
    static const UCHAR kRatePriority[] = {
        (UCHAR)VIRTIO_SND_PCM_RATE_48000,
        (UCHAR)VIRTIO_SND_PCM_RATE_44100,
        (UCHAR)VIRTIO_SND_PCM_RATE_96000,
        (UCHAR)VIRTIO_SND_PCM_RATE_88200,
        (UCHAR)VIRTIO_SND_PCM_RATE_192000,
        (UCHAR)VIRTIO_SND_PCM_RATE_176400,
        (UCHAR)VIRTIO_SND_PCM_RATE_384000,
        (UCHAR)VIRTIO_SND_PCM_RATE_64000,
        (UCHAR)VIRTIO_SND_PCM_RATE_32000,
        (UCHAR)VIRTIO_SND_PCM_RATE_22050,
        (UCHAR)VIRTIO_SND_PCM_RATE_16000,
        (UCHAR)VIRTIO_SND_PCM_RATE_11025,
        (UCHAR)VIRTIO_SND_PCM_RATE_8000,
        (UCHAR)VIRTIO_SND_PCM_RATE_5512,
    };

    ULONG chMin;
    ULONG chMax;
    UCHAR preferredChannels;
    UCHAR chosenChannels;
    UCHAR chosenFormat;
    UCHAR chosenRate;
    ULONG i;

    if (OutConfig != NULL) {
        RtlZeroMemory(OutConfig, sizeof(*OutConfig));
    }

    if (Info == NULL || OutConfig == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!VirtioSndCtrlIsValidStreamId(StreamId)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Info->stream_id != StreamId) {
        return STATUS_INVALID_PARAMETER;
    }

    if (StreamId == VIRTIO_SND_PLAYBACK_STREAM_ID) {
        if (Info->direction != VIRTIO_SND_D_OUTPUT) {
            return STATUS_INVALID_PARAMETER;
        }
    } else {
        if (Info->direction != VIRTIO_SND_D_INPUT) {
            return STATUS_INVALID_PARAMETER;
        }
    }

    preferredChannels = VirtioSndCtrlFixedChannelsForStream(StreamId);

    if (Info->formats == 0 || Info->rates == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    chMin = (Info->channels_min == 0) ? 1u : (ULONG)Info->channels_min;
    chMax = (ULONG)Info->channels_max;
    if (chMax < chMin) {
        return STATUS_NOT_SUPPORTED;
    }
    if (chMin > 8u) {
        return STATUS_NOT_SUPPORTED;
    }
    if (chMax > 8u) {
        chMax = 8u;
    }

    chosenChannels = preferredChannels;
    if ((ULONG)chosenChannels < chMin || (ULONG)chosenChannels > chMax) {
        chosenChannels = (UCHAR)chMin;
    }
    if (chosenChannels == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    chosenFormat = 0;
    for (i = 0; i < RTL_NUMBER_OF(kFormatPriority); ++i) {
        const UCHAR candidate = kFormatPriority[i];
        USHORT bytesPerSample;

        if ((Info->formats & VIRTIO_SND_PCM_FMT_MASK(candidate)) == 0) {
            continue;
        }
        if (!VirtioSndCtrlIsSupportedVirtioPcmFormat(candidate)) {
            continue;
        }

        bytesPerSample = 0;
        if (!VirtioSndPcmFormatToBytesPerSample(candidate, &bytesPerSample) || bytesPerSample == 0) {
            continue;
        }

        chosenFormat = candidate;
        break;
    }
    if (chosenFormat == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    chosenRate = 0;
    for (i = 0; i < RTL_NUMBER_OF(kRatePriority); ++i) {
        const UCHAR candidate = kRatePriority[i];
        ULONG rateHz;

        if ((Info->rates & VIRTIO_SND_PCM_RATE_MASK(candidate)) == 0) {
            continue;
        }

        rateHz = 0;
        if (!VirtioSndPcmRateToHz(candidate, &rateHz) || rateHz == 0) {
            continue;
        }

        chosenRate = candidate;
        break;
    }
    if (chosenRate == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    OutConfig->Channels = chosenChannels;
    OutConfig->Format = chosenFormat;
    OutConfig->Rate = chosenRate;
    return STATUS_SUCCESS;
}

static __forceinline BOOLEAN VirtioSndCtrlIsSupportedPcmFormat(_In_ UCHAR Format, _Out_opt_ USHORT* BytesPerSample)
{
    /*
     * Keep this helper intentionally minimal: it is used to compute frame sizing
     * for request validation. Higher layers (WaveRT) decide which of these
     * formats to expose to Windows.
     */
    return VirtioSndPcmFormatToBytesPerSample(Format, BytesPerSample);
}

_Use_decl_annotations_
NTSTATUS VirtioSndCtrlBuildPcmInfoReq(VIRTIO_SND_PCM_INFO_REQ* Req)
{
    if (Req == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Req, sizeof(*Req));
    Req->code = VIRTIO_SND_R_PCM_INFO;
    Req->start_id = 0;
    Req->count = 2;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS VirtioSndCtrlParsePcmInfoResp(const void* Resp, ULONG RespLen, VIRTIO_SND_PCM_INFO* PlaybackInfo, VIRTIO_SND_PCM_INFO* CaptureInfo)
{
    ULONG virtioStatus;
    VIRTIO_SND_PCM_INFO info0;
    VIRTIO_SND_PCM_INFO info1;

    if (Resp == NULL || PlaybackInfo == NULL || CaptureInfo == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (RespLen < sizeof(VIRTIO_SND_HDR_RESP)) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }

    /*
     * The response begins with a 32-bit virtio-snd status value.
     * Use memcpy so this logic is safe on hosts that dislike unaligned reads.
     */
    RtlCopyMemory(&virtioStatus, Resp, sizeof(virtioStatus));

    if (virtioStatus != VIRTIO_SND_S_OK) {
        return VirtioSndStatusToNtStatus(virtioStatus);
    }

    if (RespLen < sizeof(VIRTIO_SND_HDR_RESP) + (sizeof(VIRTIO_SND_PCM_INFO) * 2)) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }

    RtlCopyMemory(&info0, (const UCHAR*)Resp + sizeof(VIRTIO_SND_HDR_RESP), sizeof(info0));
    RtlCopyMemory(&info1, (const UCHAR*)Resp + sizeof(VIRTIO_SND_HDR_RESP) + sizeof(VIRTIO_SND_PCM_INFO), sizeof(info1));

    if (info0.stream_id != VIRTIO_SND_PLAYBACK_STREAM_ID || info1.stream_id != VIRTIO_SND_CAPTURE_STREAM_ID) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }

    if (info0.direction != VIRTIO_SND_D_OUTPUT || info1.direction != VIRTIO_SND_D_INPUT) {
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }

    /*
     * Basic sanity checks on advertised capabilities.
     *
     * The full negotiation/selection logic lives in the control engine (during
     * START_DEVICE) so it can emit detailed trace logs on failure.
     */
    if (info0.formats == 0 || info0.rates == 0 || info1.formats == 0 || info1.rates == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    /*
     * Treat channels_min==0 as "1" for robustness (matches WaveRT capability
     * enumeration handling).
     */
    if (info0.channels_max < ((info0.channels_min == 0) ? 1u : info0.channels_min) ||
        info1.channels_max < ((info1.channels_min == 0) ? 1u : info1.channels_min)) {
        return STATUS_NOT_SUPPORTED;
    }

    *PlaybackInfo = info0;
    *CaptureInfo = info1;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS VirtioSndCtrlBuildPcmSetParamsReq(
    VIRTIO_SND_PCM_SET_PARAMS_REQ* Req,
    ULONG StreamId,
    ULONG BufferBytes,
    ULONG PeriodBytes)
{
    UCHAR channels;
    ULONG frameBytes;

    if (Req == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!VirtioSndCtrlIsValidStreamId(StreamId)) {
        return STATUS_INVALID_PARAMETER;
    }

    channels = VirtioSndCtrlFixedChannelsForStream(StreamId);
    frameBytes = (ULONG)channels * 2u; /* S16_LE => 2 bytes per sample */
    if (frameBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Validate period sizing up-front so callers don't accidentally submit
     * misaligned PCM buffers.
     */
    if (BufferBytes == 0 || PeriodBytes == 0 || PeriodBytes > BufferBytes || (BufferBytes % frameBytes) != 0 || (PeriodBytes % frameBytes) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Contract v1 requires the device to reject any single PCM transfer whose
     * PCM payload exceeds 256 KiB (262,144 bytes) with VIRTIO_SND_S_BAD_MSG.
     * Reject these up-front so callers don't accidentally break streaming by
     * triggering fatal BAD_MSG handling in the TX/RX engines.
     */
    if (PeriodBytes > VIRTIOSND_MAX_PCM_PAYLOAD_BYTES) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    /*
     * The driver allocates a cyclic DMA buffer of BufferBytes (WaveRT ring
     * buffer). Cap it to a reasonable maximum to avoid unbounded nonpaged
     * contiguous allocations via user-mode latency/buffering requests.
     */
    if (BufferBytes > VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    RtlZeroMemory(Req, sizeof(*Req));
    Req->code = VIRTIO_SND_R_PCM_SET_PARAMS;
    Req->stream_id = StreamId;
    Req->buffer_bytes = BufferBytes;
    Req->period_bytes = PeriodBytes;
    Req->features = 0;
    Req->channels = channels;
    Req->format = (UCHAR)VIRTIO_SND_PCM_FMT_S16;
    Req->rate = (UCHAR)VIRTIO_SND_PCM_RATE_48000;
    Req->padding = 0;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS VirtioSndCtrlBuildPcmSetParamsReqEx(
    VIRTIO_SND_PCM_SET_PARAMS_REQ* Req,
    ULONG StreamId,
    ULONG BufferBytes,
    ULONG PeriodBytes,
    UCHAR Channels,
    UCHAR Format,
    UCHAR Rate)
{
    USHORT bytesPerSample;
    ULONG frameBytes;
    ULONG rateHz;

    if (Req == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!VirtioSndCtrlIsValidStreamId(StreamId)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Channels == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    bytesPerSample = 0;
    if (!VirtioSndCtrlIsSupportedPcmFormat(Format, &bytesPerSample) || bytesPerSample == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    rateHz = 0;
    if (!VirtioSndPcmRateToHz(Rate, &rateHz) || rateHz == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    frameBytes = (ULONG)Channels * (ULONG)bytesPerSample;
    if (frameBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Validate buffer/period sizing up-front so callers don't accidentally
     * submit misaligned PCM buffers.
     */
    if (BufferBytes == 0 || PeriodBytes == 0 || PeriodBytes > BufferBytes || (BufferBytes % frameBytes) != 0 || (PeriodBytes % frameBytes) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PeriodBytes > VIRTIOSND_MAX_PCM_PAYLOAD_BYTES) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    if (BufferBytes > VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    RtlZeroMemory(Req, sizeof(*Req));
    Req->code = VIRTIO_SND_R_PCM_SET_PARAMS;
    Req->stream_id = StreamId;
    Req->buffer_bytes = BufferBytes;
    Req->period_bytes = PeriodBytes;
    Req->features = 0;
    Req->channels = Channels;
    Req->format = Format;
    Req->rate = Rate;
    Req->padding = 0;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS VirtioSndCtrlBuildPcmSimpleReq(VIRTIO_SND_PCM_SIMPLE_REQ* Req, ULONG StreamId, ULONG Code)
{
    if (Req == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!VirtioSndCtrlIsValidStreamId(StreamId)) {
        return STATUS_INVALID_PARAMETER;
    }

    switch (Code) {
    case VIRTIO_SND_R_PCM_PREPARE:
    case VIRTIO_SND_R_PCM_RELEASE:
    case VIRTIO_SND_R_PCM_START:
    case VIRTIO_SND_R_PCM_STOP:
        break;
    default:
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Req, sizeof(*Req));
    Req->code = Code;
    Req->stream_id = StreamId;
    return STATUS_SUCCESS;
}
