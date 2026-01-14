/*
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * virtio-net TX header builder for checksum offload + TSO/GSO.
 *
 * This file is intentionally NDIS-free so it can be compiled in host-side tests.
 */

#include "../include/aero_virtio_net_offload.h"
#include "../include/virtio_net_hdr_offload.h"

#include <string.h>

AEROVNET_OFFLOAD_RESULT AerovNetBuildTxVirtioNetHdr(const uint8_t* frame,
                                                    size_t frame_len,
                                                    const AEROVNET_TX_OFFLOAD_INTENT* intent,
                                                    AEROVNET_VIRTIO_NET_HDR* out_hdr,
                                                    AEROVNET_OFFLOAD_PARSE_INFO* out_info) {
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;
  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO FrameInfo;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Built;

  if (!out_hdr) {
    return AEROVNET_OFFLOAD_ERR_INVAL;
  }

  memset(out_hdr, 0, sizeof(*out_hdr));
  if (out_info) {
    memset(out_info, 0, sizeof(*out_info));
  }

  if (!intent || (intent->WantTcpChecksum == 0 && intent->WantUdpChecksum == 0 && intent->WantTso == 0)) {
    return AEROVNET_OFFLOAD_OK;
  }

  /* Only one L4 checksum type may be requested for a given frame. */
  if ((intent->WantTcpChecksum != 0 && intent->WantUdpChecksum != 0) || (intent->WantTso != 0 && intent->WantUdpChecksum != 0)) {
    return AEROVNET_OFFLOAD_ERR_INVAL;
  }

  memset(&FrameInfo, 0, sizeof(FrameInfo));
  St = VirtioNetHdrOffloadParseFrameHeaders(frame, frame_len, &FrameInfo);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    if (St == VIRTIO_NET_HDR_OFFLOAD_STATUS_INVALID_ARGUMENT) {
      return AEROVNET_OFFLOAD_ERR_INVAL;
    }
    if (St == VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED || St == VIRTIO_NET_HDR_OFFLOAD_STATUS_MALFORMED) {
      return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
    }
    return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_ETHERTYPE;
  }

  if (FrameInfo.IsFragmented) {
    return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_FRAGMENTATION;
  }

  if (FrameInfo.L3Proto != (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV4 && FrameInfo.L3Proto != (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV6) {
    return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_ETHERTYPE;
  }

  /* Enforce the requested checksum type against the parsed L4 protocol. */
  if (intent->WantTso != 0 || intent->WantTcpChecksum != 0) {
    if (FrameInfo.L4Proto != 6u) {
      return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_L4_PROTOCOL;
    }
  } else if (intent->WantUdpChecksum != 0) {
    if (FrameInfo.L4Proto != 17u) {
      return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_L4_PROTOCOL;
    }
  }

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.NeedsCsum = 1;
  TxReq.Tso = (intent->WantTso != 0) ? 1u : 0u;
  TxReq.TsoEcn = 0;
  TxReq.TsoMss = intent->TsoMss;
  if (TxReq.Tso != 0 && intent->TsoEcn != 0) {
    /*
     * virtio-net ECN handling (VIRTIO_NET_F_HOST_ECN):
     * only set the ECN bit for TSO packets where the original TCP header has CWR set.
     *
     * `VirtioNetHdrOffloadParseFrameHeaders` already validated that the TCP header is
     * present in the provided buffer.
     */
    size_t FlagsOff = (size_t)FrameInfo.L4Offset + 13u;
    if (FlagsOff < frame_len) {
      uint8_t TcpFlags = frame[FlagsOff];
      if ((TcpFlags & 0x80u) != 0) { /* CWR */
        TxReq.TsoEcn = 1u;
      }
    }
  }

  memset(&Built, 0, sizeof(Built));
  St = VirtioNetHdrOffloadBuildTxHdr(&FrameInfo, &TxReq, &Built);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    if (St == VIRTIO_NET_HDR_OFFLOAD_STATUS_INVALID_ARGUMENT) {
      return (intent->WantTso != 0 && intent->TsoMss == 0) ? AEROVNET_OFFLOAD_ERR_BAD_MSS : AEROVNET_OFFLOAD_ERR_INVAL;
    }
    return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_L4_PROTOCOL;
  }

  /* Copy the built header into the driver's portable header struct. */
  memcpy(out_hdr, &Built, sizeof(*out_hdr));

  if (out_info) {
    out_info->IpVersion = FrameInfo.L3Proto;
    out_info->L4Protocol = FrameInfo.L4Proto;
    out_info->L2Len = FrameInfo.L2Len;
    out_info->L3Len = FrameInfo.L3Len;
    out_info->L4Len = FrameInfo.L4Len;
    out_info->L4Offset = FrameInfo.L4Offset;
    out_info->HeadersLen = FrameInfo.PayloadOffset;
  }

  return AEROVNET_OFFLOAD_OK;
}
