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
 * Default contract-v1 stream parameters (safe baseline).
 */
#define VIRTIOSND_PCM_DEFAULT_FORMAT VIRTIO_SND_PCM_FMT_S16
#define VIRTIOSND_PCM_DEFAULT_RATE   VIRTIO_SND_PCM_RATE_48000

/*
 * Driver-supported PCM capabilities (subset of the virtio-snd spec).
 *
 * Devices may advertise additional formats/rates, but the Windows 7 WaveRT
 * miniport only enumerates/negotiates formats/rates that it can represent via
 * KS/WAVEFORMATEXTENSIBLE.
 */
#define VIRTIOSND_PCM_DRIVER_SUPPORTED_FORMATS (                                                               \
    VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_U8) | VIRTIO_SND_PCM_FMT_MASK_S16 | VIRTIO_SND_PCM_FMT_MASK_S24 | \
    VIRTIO_SND_PCM_FMT_MASK_S32 | VIRTIO_SND_PCM_FMT_MASK_FLOAT | VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_FLOAT64))

#define VIRTIOSND_PCM_DRIVER_SUPPORTED_RATES (                                                                                              \
    VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_5512) | VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_8000) |                               \
    VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_11025) | VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_16000) |                             \
    VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_22050) | VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_32000) |                             \
    VIRTIO_SND_PCM_RATE_MASK_44100 | VIRTIO_SND_PCM_RATE_MASK_48000 | VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_64000) |                 \
    VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_88200) | VIRTIO_SND_PCM_RATE_MASK_96000 | VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_176400) | \
    VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_192000) | VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_384000))

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
 * Select a deterministic "best" (channels, format, rate) tuple from a device's
 * advertised PCM_INFO.
 *
 * This is used during START_DEVICE (VIO-020) to pick the initial/default stream
 * configuration (preferring the contract-v1 baseline). The WaveRT miniport may
 * later update the selected stream configuration if Windows opens a stream in a
 * different supported format.
 *
 * Selection policy:
 *  - Prefer the legacy Aero contract v1 default when available:
 *      - playback: 2ch, S16, 48kHz
 *      - capture:  1ch, S16, 48kHz
 *  - Otherwise pick the first supported entry from these priority lists:
 *      formats: S16, S24, S32, FLOAT, FLOAT64, U8
 *      rates:   48kHz, 44.1kHz, 96kHz, 88.2kHz, 192kHz, 176.4kHz, 384kHz,
 *              64kHz, 32kHz, 22.05kHz, 16kHz, 11.025kHz, 8kHz, 5.512kHz
 *  - Channels are selected as the stream's preferred channel count (2 playback,
 *    1 capture) if it falls within the device-advertised range. Otherwise, the
 *    lowest supported channel count is chosen.
 *
 * Returns STATUS_SUCCESS on success or STATUS_NOT_SUPPORTED if no supported
 * configuration exists.
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
 * PCM stream (S16/48kHz).
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
 * Callers should validate that the chosen Channels/Format/Rate are present in
 * the device's PCM_INFO capabilities (formats/rates bitmasks and channel
 * ranges).
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
 * Map virtio-snd PCM enums to concrete PCM properties.
 *
 * Returns FALSE for formats/rates not handled by this driver.
 */
_Must_inspect_result_ BOOLEAN VirtioSndPcmFormatToBytes(_In_ UCHAR Format, _Out_ USHORT* BytesPerSample, _Out_opt_ USHORT* BitsPerSample);
_Must_inspect_result_ BOOLEAN VirtioSndPcmHzToRate(_In_ ULONG Hz, _Out_ UCHAR* Rate);
_Must_inspect_result_ BOOLEAN VirtioSndPcmBitsToFormat(_In_ USHORT BitsPerSample, _In_ BOOLEAN IsFloat, _Out_ UCHAR* Format);

/*
 * Select a virtio-snd PCM format/rate to use for a given Windows audio request.
 *
 * If the requested combination is not supported, this falls back to the
 * contract-v1 baseline (S16/48kHz), assuming it is present in the supplied
 * SupportedFormats/SupportedRates masks.
 */
_Must_inspect_result_ NTSTATUS VirtioSndPcmSelectFormatRate(
    _In_ ULONGLONG SupportedFormats,
    _In_ ULONGLONG SupportedRates,
    _In_ USHORT RequestedBitsPerSample,
    _In_ ULONG RequestedSampleRate,
    _In_ BOOLEAN RequestedFloat,
    _Out_ UCHAR* OutFormat,
    _Out_ UCHAR* OutRate);

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
