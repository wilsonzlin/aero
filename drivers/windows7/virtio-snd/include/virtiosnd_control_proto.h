/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"

/*
 * Host-testable control-plane protocol helpers.
 *
 * The virtiosnd_control.c engine is tightly coupled to WDM (DMA adapters,
 * events, spinlocks, etc). These helpers isolate the contract-v1 message
 * framing and validation so it can be unit tested on a normal host toolchain.
 *
 * All structures are the packed wire format from virtio_snd_proto.h. The
 * Windows 7 guest environment is little-endian so the driver writes native
 * integer values directly.
 */

#ifdef __cplusplus
extern "C" {
#endif

/*
 * A single negotiated PCM configuration for a virtio-snd stream.
 *
 * The driver tracks a "selected" configuration per stream:
 *
 * - A default configuration is selected at device start time from `PCM_INFO`
 *   (preferring the contract-v1 baseline).
 * - The WaveRT miniport may update the selection when Windows opens a stream in
 *   a different supported format/rate/channel count.
 *
 * The selected configuration is used when building `VIRTIO_SND_R_PCM_SET_PARAMS`
 * requests.
 */
typedef struct _VIRTIOSND_PCM_CONFIG {
    UCHAR Channels;
    UCHAR Format; /* VIRTIO_SND_PCM_FMT_* */
    UCHAR Rate;   /* VIRTIO_SND_PCM_RATE_* */
} VIRTIOSND_PCM_CONFIG;

/*
 * Select a (format, rate, channels) triple from a device's advertised PCM_INFO
 * masks.
 *
 * Deterministic selection policy (in priority order):
 *   1) S16 @ 48kHz  (legacy contract v1 default)
 *   2) S16 @ 44.1kHz
 *   3) S24 @ 48kHz  (24-bit samples in a 32-bit container)
 *   4) S24 @ 44.1kHz
 *   5) S32 @ 48kHz
 *   6) S32 @ 44.1kHz
 *
 * Channel policy (default selection):
 *   - Playback stream 0: require stereo (2ch)
 *   - Capture stream 1: require mono (1ch)
 *
 * If no supported combination exists, returns STATUS_NOT_SUPPORTED.
 */
_Must_inspect_result_ NTSTATUS VirtioSndCtrlSelectPcmConfig(
    _In_ const VIRTIO_SND_PCM_INFO* Info,
    _In_ ULONG StreamId,
    _Out_ VIRTIOSND_PCM_CONFIG* OutConfig);

/*
 * Build a VIRTIO_SND_R_PCM_INFO request for the contract-v1 (two fixed streams:
 * 0 playback + 1 capture).
 */
_Must_inspect_result_ NTSTATUS VirtioSndCtrlBuildPcmInfoReq(_Out_ VIRTIO_SND_PCM_INFO_REQ* Req);

/*
 * Parse a VIRTIO_SND_R_PCM_INFO response payload.
 *
 * Resp points at the raw device-written response bytes beginning with the
 * status field (VIRTIO_SND_HDR_RESP).
 */
_Must_inspect_result_ NTSTATUS VirtioSndCtrlParsePcmInfoResp(
    _In_reads_bytes_(RespLen) const void* Resp,
    _In_ ULONG RespLen,
    _Out_ VIRTIO_SND_PCM_INFO* PlaybackInfo,
    _Out_ VIRTIO_SND_PCM_INFO* CaptureInfo);

/*
 * Build a VIRTIO_SND_R_PCM_SET_PARAMS request for a fixed-format contract-v1
 * PCM stream.
 */
_Must_inspect_result_ NTSTATUS VirtioSndCtrlBuildPcmSetParamsReq(
    _Out_ VIRTIO_SND_PCM_SET_PARAMS_REQ* Req,
    _In_ ULONG StreamId,
    _In_ ULONG BufferBytes,
    _In_ ULONG PeriodBytes);

/*
 * Build a VIRTIO_SND_R_PCM_SET_PARAMS request for an explicitly selected
 * (channels, format, rate) tuple.
 *
 * This is used by the WaveRT miniport format negotiation path. The legacy
 * VirtioSndCtrlBuildPcmSetParamsReq() helper remains fixed-format for the Aero
 * v1 contract and is used by unit tests.
 */
_Must_inspect_result_ NTSTATUS VirtioSndCtrlBuildPcmSetParamsReqEx(
    _Out_ VIRTIO_SND_PCM_SET_PARAMS_REQ* Req,
    _In_ ULONG StreamId,
    _In_ ULONG BufferBytes,
    _In_ ULONG PeriodBytes,
    _In_ UCHAR Channels,
    _In_ UCHAR Format,
    _In_ UCHAR Rate);

/*
 * Build a simple PCM control request (PREPARE/START/STOP/RELEASE).
 */
_Must_inspect_result_ NTSTATUS VirtioSndCtrlBuildPcmSimpleReq(
    _Out_ VIRTIO_SND_PCM_SIMPLE_REQ* Req,
    _In_ ULONG StreamId,
    _In_ ULONG Code);

#ifdef __cplusplus
} /* extern "C" */
#endif
