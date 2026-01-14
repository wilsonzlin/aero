/*
 * Portable virtio-net header/offload helpers.
 *
 * This code is shared between the Windows driver and host-side unit tests and
 * must remain WDK/NDIS-free.
 */

#include "../include/virtio_net_hdr_offload.h"

static void VirtioNetHdrOffloadMemset(void* Dst, int C, size_t N) {
  uint8_t* P;
  P = (uint8_t*)Dst;
  while (N != 0) {
    *P++ = (uint8_t)C;
    N--;
  }
}

static uint16_t VirtioNetHdrOffloadReadBe16(const uint8_t* P) {
  return (uint16_t)(((uint16_t)P[0] << 8) | (uint16_t)P[1]);
}

static VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadBoundsCheck(size_t Offset, size_t Need, size_t Len) {
  if (Offset > Len) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED;
  }
  if (Need > Len - Offset) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED;
  }
  return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
}

static int VirtioNetHdrOffloadIsVlanEthertype(uint16_t EtherType) {
  /*
   * Common VLAN ethertypes:
   * - 0x8100: 802.1Q
   * - 0x88A8: 802.1ad (QinQ / provider bridging)
   *
   * Some environments also use 0x9100; treat it as VLAN as well for robustness.
   */
  return (EtherType == 0x8100u || EtherType == 0x88A8u || EtherType == 0x9100u) ? 1 : 0;
}

void VirtioNetHdrOffloadZero(VIRTIO_NET_HDR* Hdr) {
  if (Hdr != NULL) {
    VirtioNetHdrOffloadMemset(Hdr, 0, sizeof(*Hdr));
  }
}

static VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseTcp(const uint8_t* Frame, size_t FrameLen, size_t L4Offset,
                                                                 VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info) {
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;
  const uint8_t* Tcp;
  uint8_t DataOffsetWords;
  size_t TcpHdrLen;

  St = VirtioNetHdrOffloadBoundsCheck(L4Offset, 20, FrameLen);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return St;
  }

  Tcp = Frame + L4Offset;
  DataOffsetWords = (uint8_t)(Tcp[12] >> 4);
  TcpHdrLen = (size_t)DataOffsetWords * 4u;
  if (TcpHdrLen < 20u) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_MALFORMED;
  }

  St = VirtioNetHdrOffloadBoundsCheck(L4Offset, TcpHdrLen, FrameLen);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return St;
  }

  Info->L4Len = (uint16_t)TcpHdrLen;
  Info->PayloadOffset = (uint16_t)(L4Offset + TcpHdrLen);

  Info->CsumStart = (uint16_t)L4Offset;
  Info->CsumOffset = 16u; /* TCP checksum field */
  return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
}

static VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseUdp(const uint8_t* Frame, size_t FrameLen, size_t L4Offset,
                                                                 VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info) {
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  (void)Frame;

  St = VirtioNetHdrOffloadBoundsCheck(L4Offset, 8, FrameLen);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return St;
  }

  Info->L4Len = 8u;
  Info->PayloadOffset = (uint16_t)(L4Offset + 8u);
  Info->CsumStart = (uint16_t)L4Offset;
  Info->CsumOffset = 6u; /* UDP checksum field */
  return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
}

static VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseIpv4(const uint8_t* Frame, size_t FrameLen, size_t L3Offset,
                                                                  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info, int StrictLength) {
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;
  const uint8_t* Ip;
  uint8_t Version;
  uint8_t IhlWords;
  size_t IpHdrLen;
  uint16_t TotalLen;
  size_t MaxEnd;
  uint16_t FragOffFlags;
  uint16_t FragOff;
  uint16_t MoreFrags;
  size_t L4Offset;

  St = VirtioNetHdrOffloadBoundsCheck(L3Offset, 20, FrameLen);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return St;
  }

  Ip = Frame + L3Offset;
  Version = (uint8_t)(Ip[0] >> 4);
  if (Version != 4u) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_MALFORMED;
  }

  IhlWords = (uint8_t)(Ip[0] & 0x0Fu);
  IpHdrLen = (size_t)IhlWords * 4u;
  if (IpHdrLen < 20u) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_MALFORMED;
  }

  St = VirtioNetHdrOffloadBoundsCheck(L3Offset, IpHdrLen, FrameLen);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return St;
  }

  TotalLen = VirtioNetHdrOffloadReadBe16(Ip + 2);
  if (TotalLen == 0) {
    /*
     * IPv4 total_len must be non-zero, but allow `total_len == 0` in
     * header-only parsing mode for robustness (treat it as "unknown" and bound
     * parsing by the available bytes). Strict parsing still rejects this.
     */
    if (StrictLength) {
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_MALFORMED;
    }
    MaxEnd = FrameLen;
  } else {
    if (TotalLen < (uint16_t)IpHdrLen) {
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_MALFORMED;
    }
    if (StrictLength && (size_t)TotalLen > FrameLen - L3Offset) {
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED;
    }

    {
      size_t PacketEnd = L3Offset + (size_t)TotalLen;
      MaxEnd = (PacketEnd < FrameLen) ? PacketEnd : FrameLen;
    }
  }

  Info->L3Proto = (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV4;
  Info->L3Offset = (uint16_t)L3Offset;
  Info->L3Len = (uint16_t)IpHdrLen;
  Info->L4Proto = Ip[9];

  L4Offset = L3Offset + IpHdrLen;
  Info->L4Offset = (uint16_t)L4Offset;

  FragOffFlags = VirtioNetHdrOffloadReadBe16(Ip + 6);
  FragOff = (uint16_t)(FragOffFlags & 0x1FFFu);
  MoreFrags = (uint16_t)(FragOffFlags & 0x2000u);

  if (FragOff != 0u || MoreFrags != 0u) {
    Info->IsFragmented = 1u;
  }

  /*
   * L4 header is only present in the first fragment. For non-first fragments,
   * stop after the IPv4 header.
   */
  if (FragOff != 0u) {
    Info->L4Len = 0;
    Info->PayloadOffset = (uint16_t)L4Offset;
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
  }

  switch (Info->L4Proto) {
    case 6: /* TCP */
      return VirtioNetHdrOffloadParseTcp(Frame, MaxEnd, L4Offset, Info);
    case 17: /* UDP */
      return VirtioNetHdrOffloadParseUdp(Frame, MaxEnd, L4Offset, Info);
    default:
      Info->L4Len = 0;
      Info->PayloadOffset = (uint16_t)L4Offset;
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
  }
}

static VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseIpv6(const uint8_t* Frame, size_t FrameLen, size_t L3Offset,
                                                                  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info, int StrictLength) {
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;
  const uint8_t* Ip;
  uint8_t Version;
  uint16_t PayloadLen;
  uint8_t NextHdr;
  size_t Offset;
  size_t MaxEnd;
  unsigned Iter;
  uint8_t NoL4;

  St = VirtioNetHdrOffloadBoundsCheck(L3Offset, 40, FrameLen);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return St;
  }

  Ip = Frame + L3Offset;
  Version = (uint8_t)(Ip[0] >> 4);
  if (Version != 6u) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_MALFORMED;
  }

  PayloadLen = VirtioNetHdrOffloadReadBe16(Ip + 4);
  /*
   * Payload length excludes the 40-byte base header. If it's non-zero, ensure
   * the packet isn't truncated. (We don't currently support jumbograms.)
   */
  if (StrictLength && (size_t)PayloadLen > FrameLen - L3Offset - 40u) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED;
  }
  {
    size_t PacketEnd = L3Offset + 40u + (size_t)PayloadLen;
    MaxEnd = (PacketEnd < FrameLen) ? PacketEnd : FrameLen;
  }

  NextHdr = Ip[6];
  Offset = L3Offset + 40u;
  NoL4 = 0;

  /*
   * Skip a bounded set of IPv6 extension headers to locate the L4 header.
   * This is intentionally conservative; unsupported or ambiguous headers
   * return UNSUPPORTED rather than guessing offsets.
   */
  for (Iter = 0; Iter < 8u; Iter++) {
    if (NextHdr == 6u || NextHdr == 17u) {
      break;
    }

    if (NextHdr == 59u) { /* No Next Header */
      break;
    }

    if (NextHdr == 0u || NextHdr == 43u || NextHdr == 60u) {
      /* Hop-by-hop, Routing, Destination Options: (Hdr Ext Len + 1) * 8 */
      const uint8_t* Ext;
      size_t ExtLen;

      St = VirtioNetHdrOffloadBoundsCheck(Offset, 2, MaxEnd);
      if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
        return St;
      }
      Ext = Frame + Offset;
      ExtLen = ((size_t)Ext[1] + 1u) * 8u;
      St = VirtioNetHdrOffloadBoundsCheck(Offset, ExtLen, MaxEnd);
      if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
        return St;
      }
      NextHdr = Ext[0];
      Offset += ExtLen;
      continue;
    }

    if (NextHdr == 44u) {
      /* Fragment header: fixed 8 bytes */
      const uint8_t* Ext;
      uint16_t FragOffFlags;

      St = VirtioNetHdrOffloadBoundsCheck(Offset, 8, MaxEnd);
      if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
        return St;
      }
      Ext = Frame + Offset;
      FragOffFlags = VirtioNetHdrOffloadReadBe16(Ext + 2);
      Info->IsFragmented = 1u;
      /* If this isn't the first fragment (offset != 0), L4 isn't present. */
      if ((FragOffFlags & 0xFFF8u) != 0) {
        NextHdr = Ext[0];
        Offset += 8u;
        NoL4 = 1u;
        break;
      }
      NextHdr = Ext[0];
      Offset += 8u;
      continue;
    }

    if (NextHdr == 51u) {
      /* Authentication header: (Payload Len + 2) * 4 */
      const uint8_t* Ext;
      size_t ExtLen;

      St = VirtioNetHdrOffloadBoundsCheck(Offset, 2, MaxEnd);
      if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
        return St;
      }
      Ext = Frame + Offset;
      ExtLen = ((size_t)Ext[1] + 2u) * 4u;
      St = VirtioNetHdrOffloadBoundsCheck(Offset, ExtLen, MaxEnd);
      if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
        return St;
      }
      NextHdr = Ext[0];
      Offset += ExtLen;
      continue;
    }

    /* ESP and other extension headers are not safely skippable here. */
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
  }

  Info->L3Proto = (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV6;
  Info->L3Offset = (uint16_t)L3Offset;
  Info->L3Len = (uint16_t)(Offset - L3Offset);
  Info->L4Proto = NextHdr;
  Info->L4Offset = (uint16_t)Offset;

  if (NoL4) {
    Info->L4Len = 0;
    Info->PayloadOffset = (uint16_t)Offset;
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
  }

  switch (NextHdr) {
    case 6:
      return VirtioNetHdrOffloadParseTcp(Frame, MaxEnd, Offset, Info);
    case 17:
      return VirtioNetHdrOffloadParseUdp(Frame, MaxEnd, Offset, Info);
    default:
      Info->L4Len = 0;
      Info->PayloadOffset = (uint16_t)Offset;
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
  }
}

static VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseFrameInternal(const uint8_t* Frame, size_t FrameLen,
                                                                           VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info, int StrictLength) {
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;
  size_t Offset;
  uint16_t EtherType;
  unsigned VlanCount;

  if (Frame == NULL || Info == NULL) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_INVALID_ARGUMENT;
  }

  VirtioNetHdrOffloadMemset(Info, 0, sizeof(*Info));

  St = VirtioNetHdrOffloadBoundsCheck(0, 14, FrameLen);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return St;
  }

  Offset = 12u;
  EtherType = VirtioNetHdrOffloadReadBe16(Frame + Offset);
  Offset = 14u;
  VlanCount = 0;

  while (VirtioNetHdrOffloadIsVlanEthertype(EtherType)) {
    const uint8_t* Tag;
    if (VlanCount >= 2u) {
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
    }
    St = VirtioNetHdrOffloadBoundsCheck(Offset, 4, FrameLen);
    if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
      return St;
    }
    Tag = Frame + Offset;
    EtherType = VirtioNetHdrOffloadReadBe16(Tag + 2);
    Offset += 4u;
    VlanCount++;
  }

  Info->L2Len = (uint16_t)Offset;
  Info->L3Offset = (uint16_t)Offset;

  switch (EtherType) {
    case 0x0800u:
      return VirtioNetHdrOffloadParseIpv4(Frame, FrameLen, Offset, Info, StrictLength);
    case 0x86DDu:
      return VirtioNetHdrOffloadParseIpv6(Frame, FrameLen, Offset, Info, StrictLength);
    default:
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
  }
}

VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseFrame(const uint8_t* Frame, size_t FrameLen, VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info) {
  return VirtioNetHdrOffloadParseFrameInternal(Frame, FrameLen, Info, 1);
}

VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseFrameHeaders(const uint8_t* Frame, size_t FrameLen,
                                                                   VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info) {
  return VirtioNetHdrOffloadParseFrameInternal(Frame, FrameLen, Info, 0);
}

VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadBuildTxHdr(const VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info,
                                                            const VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST* TxReq, VIRTIO_NET_HDR* Hdr) {
  uint8_t BaseGso;

  if (Info == NULL || TxReq == NULL || Hdr == NULL) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_INVALID_ARGUMENT;
  }

  VirtioNetHdrOffloadMemset(Hdr, 0, sizeof(*Hdr));

  /* No offload requested: virtio header must be all zeros. */
  if (!TxReq->NeedsCsum && !TxReq->Tso) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
  }

  if (TxReq->NeedsCsum || TxReq->Tso) {
    /* Do not attempt offloads on fragmented packets. */
    if (Info->IsFragmented) {
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
    }
    if (Info->L4Proto != 6u && Info->L4Proto != 17u) {
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
    }
    if (Info->L4Len == 0u) {
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
    }
    Hdr->Flags |= VIRTIO_NET_HDR_F_NEEDS_CSUM;
    Hdr->CsumStart = Info->CsumStart;
    Hdr->CsumOffset = Info->CsumOffset;
  }

  if (!TxReq->Tso) {
    Hdr->GsoType = VIRTIO_NET_HDR_GSO_NONE;
    Hdr->GsoSize = 0;
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
  }

  if (Info->L4Proto != 6u) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
  }
  if (TxReq->TsoMss == 0u) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_INVALID_ARGUMENT;
  }

  if (Info->L3Proto == (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV4) {
    BaseGso = VIRTIO_NET_HDR_GSO_TCPV4;
  } else if (Info->L3Proto == (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV6) {
    BaseGso = VIRTIO_NET_HDR_GSO_TCPV6;
  } else {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
  }

  Hdr->GsoType = (uint8_t)(BaseGso | (TxReq->TsoEcn ? VIRTIO_NET_HDR_GSO_ECN : 0u));
  Hdr->GsoSize = TxReq->TsoMss;
  Hdr->HdrLen = Info->PayloadOffset;
  return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
}

VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadBuildTxHdrFromFrame(const uint8_t* Frame, size_t FrameLen,
                                                                     const VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST* TxReq,
                                                                     VIRTIO_NET_HDR* Hdr) {
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;
  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;

  if (TxReq == NULL || Hdr == NULL) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_INVALID_ARGUMENT;
  }

  if (!TxReq->NeedsCsum && !TxReq->Tso) {
    VirtioNetHdrOffloadZero(Hdr);
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
  }

  St = VirtioNetHdrOffloadParseFrameHeaders(Frame, FrameLen, &Info);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return St;
  }

  return VirtioNetHdrOffloadBuildTxHdr(&Info, TxReq, Hdr);
}

VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseRxHdr(const VIRTIO_NET_HDR* Hdr, VIRTIO_NET_HDR_OFFLOAD_RX_INFO* Info) {
  uint8_t GsoType;

  if (Hdr == NULL || Info == NULL) {
    return VIRTIO_NET_HDR_OFFLOAD_STATUS_INVALID_ARGUMENT;
  }

  VirtioNetHdrOffloadMemset(Info, 0, sizeof(*Info));

  Info->NeedsCsum = (Hdr->Flags & VIRTIO_NET_HDR_F_NEEDS_CSUM) ? 1u : 0u;
  Info->CsumValid = (Hdr->Flags & VIRTIO_NET_HDR_F_DATA_VALID) ? 1u : 0u;

  Info->HdrLen = Hdr->HdrLen;
  Info->GsoSize = Hdr->GsoSize;

  Info->GsoEcn = (Hdr->GsoType & VIRTIO_NET_HDR_GSO_ECN) ? 1u : 0u;
  GsoType = (uint8_t)(Hdr->GsoType & (uint8_t)~VIRTIO_NET_HDR_GSO_ECN);
  Info->GsoType = GsoType;

  Info->IsGso = (GsoType != VIRTIO_NET_HDR_GSO_NONE) ? 1u : 0u;

  switch (GsoType) {
    case VIRTIO_NET_HDR_GSO_NONE:
    case VIRTIO_NET_HDR_GSO_TCPV4:
    case VIRTIO_NET_HDR_GSO_TCPV6:
    case VIRTIO_NET_HDR_GSO_UDP:
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_OK;
    default:
      /* Unknown type; pass through but report UNSUPPORTED to the caller. */
      return VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED;
  }
}
