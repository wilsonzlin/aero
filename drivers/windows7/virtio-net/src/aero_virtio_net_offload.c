/*
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * virtio-net TX header builder for checksum offload + TSO/GSO.
 *
 * This file is intentionally NDIS-free so it can be compiled in host-side tests.
 */

#include "../include/aero_virtio_net_offload.h"

#include <string.h>

#define AEROVNET_ETH_HEADER_LEN 14u
#define AEROVNET_ETHERTYPE_IPV4 0x0800u
#define AEROVNET_ETHERTYPE_IPV6 0x86DDu
#define AEROVNET_ETHERTYPE_VLAN 0x8100u
#define AEROVNET_ETHERTYPE_QINQ 0x88A8u
#define AEROVNET_ETHERTYPE_VLAN_ALT 0x9100u

#define AEROVNET_IPPROTO_TCP 6u

static uint16_t aerovnet_read_be16(const uint8_t* p) { return (uint16_t)((uint16_t)p[0] << 8) | (uint16_t)p[1]; }

static AEROVNET_OFFLOAD_RESULT aerovnet_parse_ethernet(const uint8_t* frame,
                                                       size_t frame_len,
                                                       uint16_t* ethertype_out,
                                                       size_t* l2_len_out) {
  uint16_t ethertype;
  size_t l2_len;

  if (!frame || !ethertype_out || !l2_len_out) {
    return AEROVNET_OFFLOAD_ERR_INVAL;
  }

  if (frame_len < AEROVNET_ETH_HEADER_LEN) {
    return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
  }

  ethertype = aerovnet_read_be16(frame + 12);
  l2_len = AEROVNET_ETH_HEADER_LEN;

  /* Support stacked VLAN tags (802.1Q/QinQ). */
  while (ethertype == AEROVNET_ETHERTYPE_VLAN || ethertype == AEROVNET_ETHERTYPE_QINQ || ethertype == AEROVNET_ETHERTYPE_VLAN_ALT) {
    if (frame_len < l2_len + 4u) {
      return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
    }
    ethertype = aerovnet_read_be16(frame + l2_len + 2u);
    l2_len += 4u;
  }

  *ethertype_out = ethertype;
  *l2_len_out = l2_len;
  return AEROVNET_OFFLOAD_OK;
}

static AEROVNET_OFFLOAD_RESULT aerovnet_parse_ipv4(const uint8_t* ipv4,
                                                   size_t ipv4_len,
                                                   size_t* header_len_out,
                                                   uint8_t* proto_out) {
  uint8_t version;
  size_t ihl;

  if (!ipv4 || !header_len_out || !proto_out) {
    return AEROVNET_OFFLOAD_ERR_INVAL;
  }

  if (ipv4_len < 20u) {
    return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
  }

  version = (uint8_t)(ipv4[0] >> 4);
  if (version != 4u) {
    return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_IP_VERSION;
  }

  ihl = (size_t)(ipv4[0] & 0x0Fu) * 4u;
  if (ihl < 20u || ipv4_len < ihl) {
    return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
  }

  *header_len_out = ihl;
  *proto_out = ipv4[9];
  return AEROVNET_OFFLOAD_OK;
}

static AEROVNET_OFFLOAD_RESULT aerovnet_parse_ipv6_l4_offset(const uint8_t* ipv6,
                                                             size_t ipv6_len,
                                                             size_t* l3_len_out,
                                                             uint8_t* proto_out) {
  uint8_t version;
  uint8_t next;
  size_t off;

  if (!ipv6 || !l3_len_out || !proto_out) {
    return AEROVNET_OFFLOAD_ERR_INVAL;
  }

  if (ipv6_len < 40u) {
    return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
  }

  version = (uint8_t)(ipv6[0] >> 4);
  if (version != 6u) {
    return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_IP_VERSION;
  }

  next = ipv6[6];
  off = 40u;

  /*
   * Minimal IPv6 extension header walker. This intentionally supports only the
   * standard, length-delimited headers that Windows commonly emits.
   */
  for (;;) {
    if (next == AEROVNET_IPPROTO_TCP) {
      *l3_len_out = off;
      *proto_out = next;
      return AEROVNET_OFFLOAD_OK;
    }

    /* No Next Header / ESP are treated as unsupported. */
    if (next == 59u || next == 50u) {
      return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_IPV6;
    }

    if (next == 0u || next == 43u || next == 60u) {
      /* Hop-by-hop, Routing, Destination Options: len = (HdrExtLen+1)*8 */
      uint8_t hdr_next;
      uint8_t hdr_len;
      size_t ext_len;

      if (ipv6_len < off + 2u) {
        return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
      }
      hdr_next = ipv6[off];
      hdr_len = ipv6[off + 1u];
      ext_len = ((size_t)hdr_len + 1u) * 8u;
      if (ipv6_len < off + ext_len) {
        return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
      }
      next = hdr_next;
      off += ext_len;
      continue;
    }

    if (next == 44u) {
      /* Fragment header: fixed 8 bytes. */
      uint8_t hdr_next;
      if (ipv6_len < off + 8u) {
        return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
      }
      hdr_next = ipv6[off];
      next = hdr_next;
      off += 8u;
      continue;
    }

    if (next == 51u) {
      /* Authentication header: len = (PayloadLen+2)*4 */
      uint8_t hdr_next;
      uint8_t payload_len;
      size_t ext_len;

      if (ipv6_len < off + 2u) {
        return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
      }
      hdr_next = ipv6[off];
      payload_len = ipv6[off + 1u];
      ext_len = ((size_t)payload_len + 2u) * 4u;
      if (ipv6_len < off + ext_len) {
        return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
      }
      next = hdr_next;
      off += ext_len;
      continue;
    }

    /* Unknown/unsupported extension header. */
    return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_IPV6;
  }
}

static AEROVNET_OFFLOAD_RESULT aerovnet_parse_tcp(const uint8_t* tcp, size_t tcp_len, size_t* tcp_header_len_out) {
  size_t data_offset;

  if (!tcp || !tcp_header_len_out) {
    return AEROVNET_OFFLOAD_ERR_INVAL;
  }

  if (tcp_len < 20u) {
    return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
  }

  data_offset = (size_t)(tcp[12] >> 4) * 4u;
  if (data_offset < 20u || tcp_len < data_offset) {
    return AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT;
  }

  *tcp_header_len_out = data_offset;
  return AEROVNET_OFFLOAD_OK;
}

AEROVNET_OFFLOAD_RESULT AerovNetBuildTxVirtioNetHdr(const uint8_t* frame,
                                                    size_t frame_len,
                                                    const AEROVNET_TX_OFFLOAD_INTENT* intent,
                                                    AEROVNET_VIRTIO_NET_HDR* out_hdr,
                                                    AEROVNET_OFFLOAD_PARSE_INFO* out_info) {
  AEROVNET_OFFLOAD_RESULT res;
  uint16_t ethertype;
  size_t l2_len;
  uint8_t ip_version;
  uint8_t l4_proto;
  size_t l3_len;
  size_t l4_off;
  size_t l4_len;
  size_t headers_len;

  if (!out_hdr) {
    return AEROVNET_OFFLOAD_ERR_INVAL;
  }

  memset(out_hdr, 0, sizeof(*out_hdr));
  if (out_info) {
    memset(out_info, 0, sizeof(*out_info));
  }

  if (!intent || (intent->WantTcpChecksum == 0 && intent->WantTso == 0)) {
    return AEROVNET_OFFLOAD_OK;
  }

  res = aerovnet_parse_ethernet(frame, frame_len, &ethertype, &l2_len);
  if (res != AEROVNET_OFFLOAD_OK) {
    return res;
  }

  ip_version = 0;
  l4_proto = 0;
  l3_len = 0;
  l4_off = 0;
  l4_len = 0;

  if (ethertype == AEROVNET_ETHERTYPE_IPV4) {
    ip_version = 4u;
    res = aerovnet_parse_ipv4(frame + l2_len, frame_len - l2_len, &l3_len, &l4_proto);
    if (res != AEROVNET_OFFLOAD_OK) {
      return res;
    }
    if (l4_proto != AEROVNET_IPPROTO_TCP) {
      return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_L4_PROTOCOL;
    }
    l4_off = l2_len + l3_len;
  } else if (ethertype == AEROVNET_ETHERTYPE_IPV6) {
    ip_version = 6u;
    res = aerovnet_parse_ipv6_l4_offset(frame + l2_len, frame_len - l2_len, &l3_len, &l4_proto);
    if (res != AEROVNET_OFFLOAD_OK) {
      return res;
    }
    if (l4_proto != AEROVNET_IPPROTO_TCP) {
      return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_L4_PROTOCOL;
    }
    l4_off = l2_len + l3_len;
  } else {
    return AEROVNET_OFFLOAD_ERR_UNSUPPORTED_ETHERTYPE;
  }

  res = aerovnet_parse_tcp(frame + l4_off, frame_len - l4_off, &l4_len);
  if (res != AEROVNET_OFFLOAD_OK) {
    return res;
  }

  headers_len = l4_off + l4_len;

  if (out_info) {
    out_info->IpVersion = ip_version;
    out_info->L4Protocol = l4_proto;
    out_info->L2Len = (uint16_t)l2_len;
    out_info->L3Len = (uint16_t)l3_len;
    out_info->L4Len = (uint16_t)l4_len;
    out_info->L4Offset = (uint16_t)l4_off;
    out_info->HeadersLen = (uint16_t)headers_len;
  }

  /* Always use TCP checksum completion when any offload is requested. */
  out_hdr->Flags = AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM;
  out_hdr->CsumStart = (uint16_t)l4_off;
  out_hdr->CsumOffset = 16u; /* TCP checksum field offset */

  if (intent->WantTso != 0) {
    if (intent->TsoMss == 0) {
      return AEROVNET_OFFLOAD_ERR_BAD_MSS;
    }

    out_hdr->GsoSize = intent->TsoMss;
    out_hdr->HdrLen = (uint16_t)headers_len;
    out_hdr->GsoType = (ip_version == 4u) ? AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4 : AEROVNET_VIRTIO_NET_HDR_GSO_TCPV6;
  }

  return AEROVNET_OFFLOAD_OK;
}

