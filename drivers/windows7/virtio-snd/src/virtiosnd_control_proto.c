/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_control_proto.h"
#include "virtiosnd_limits.h"

static __forceinline BOOLEAN VirtioSndCtrlIsValidStreamId(_In_ ULONG StreamId)
{
    return (StreamId == VIRTIO_SND_PLAYBACK_STREAM_ID || StreamId == VIRTIO_SND_CAPTURE_STREAM_ID) ? TRUE : FALSE;
}

static __forceinline UCHAR VirtioSndCtrlFixedChannelsForStream(_In_ ULONG StreamId)
{
    return (StreamId == VIRTIO_SND_CAPTURE_STREAM_ID) ? 1 : 2;
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

    if ((info0.formats & VIRTIO_SND_PCM_FMT_MASK_S16) == 0 || (info0.rates & VIRTIO_SND_PCM_RATE_MASK_48000) == 0 ||
        (info1.formats & VIRTIO_SND_PCM_FMT_MASK_S16) == 0 || (info1.rates & VIRTIO_SND_PCM_RATE_MASK_48000) == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    if (info0.channels_min > 2 || info0.channels_max < 2) {
        return STATUS_NOT_SUPPORTED;
    }
    if (info1.channels_min > 1 || info1.channels_max < 1) {
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
     * Contract v1 allows the device to reject a single PCM transfer larger than
     * 4 MiB with VIRTIO_SND_S_BAD_MSG. Reject these up-front so callers don't
     * accidentally break streaming by triggering fatal BAD_MSG handling in the
     * TX/RX engines.
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
