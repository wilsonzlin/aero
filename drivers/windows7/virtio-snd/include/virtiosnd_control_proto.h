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
 * Build a simple PCM control request (PREPARE/START/STOP/RELEASE).
 */
_Must_inspect_result_ NTSTATUS VirtioSndCtrlBuildPcmSimpleReq(
    _Out_ VIRTIO_SND_PCM_SIMPLE_REQ* Req,
    _In_ ULONG StreamId,
    _In_ ULONG Code);

#ifdef __cplusplus
} /* extern "C" */
#endif

