#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "virtio_net_hdr_offload.h"

#define ASSERT_TRUE(expr)                                                                                                               \
  do {                                                                                                                                 \
    if (!(expr)) {                                                                                                                     \
      fprintf(stderr, "ASSERT_TRUE failed: %s (%s:%d)\n", #expr, __FILE__, __LINE__);                                                  \
      return 1;                                                                                                                        \
    }                                                                                                                                  \
  } while (0)

#define ASSERT_EQ_INT(a, b)                                                                                                             \
  do {                                                                                                                                 \
    int _va = (int)(a);                                                                                                                 \
    int _vb = (int)(b);                                                                                                                 \
    if (_va != _vb) {                                                                                                                   \
      fprintf(stderr, "ASSERT_EQ_INT failed: %s=%d %s=%d (%s:%d)\n", #a, _va, #b, _vb, __FILE__, __LINE__);                            \
      return 1;                                                                                                                        \
    }                                                                                                                                  \
  } while (0)

#define ASSERT_EQ_U16(a, b)                                                                                                             \
  do {                                                                                                                                 \
    uint16_t _va = (uint16_t)(a);                                                                                                       \
    uint16_t _vb = (uint16_t)(b);                                                                                                       \
    if (_va != _vb) {                                                                                                                   \
      fprintf(stderr, "ASSERT_EQ_U16 failed: %s=%u %s=%u (%s:%d)\n", #a, (unsigned)_va, #b, (unsigned)_vb, __FILE__, __LINE__);       \
      return 1;                                                                                                                        \
    }                                                                                                                                  \
  } while (0)

#define ASSERT_EQ_U8(a, b)                                                                                                              \
  do {                                                                                                                                 \
    uint8_t _va = (uint8_t)(a);                                                                                                         \
    uint8_t _vb = (uint8_t)(b);                                                                                                         \
    if (_va != _vb) {                                                                                                                   \
      fprintf(stderr, "ASSERT_EQ_U8 failed: %s=%u %s=%u (%s:%d)\n", #a, (unsigned)_va, #b, (unsigned)_vb, __FILE__, __LINE__);        \
      return 1;                                                                                                                        \
    }                                                                                                                                  \
  } while (0)

static int test_ipv4_tcp_no_vlan(void) {
  /* Ethernet + IPv4 + TCP + 4-byte payload */
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype IPv4 */
      0x08, 0x00,
      /* IPv4 header */
      0x45, 0x00, 0x00, 0x2c, /* v4 ihl=5, total_len=44 */
      0x00, 0x00, 0x40, 0x00, /* id, flags/frag */
      0x40, 0x06, 0x00, 0x00, /* ttl=64, proto=TCP */
      0xc0, 0x00, 0x02, 0x01, /* src */
      0xc6, 0x33, 0x64, 0x02, /* dst */
      /* TCP header */
      0x1f, 0x90, 0x00, 0x50, /* ports */
      0x00, 0x00, 0x00, 0x00, /* seq */
      0x00, 0x00, 0x00, 0x00, /* ack */
      0x50, 0x02, 0x00, 0x00, /* doff=5, flags=SYN */
      0x00, 0x00, 0x00, 0x00, /* csum, urg */
      /* payload */
      't',  'e',  's',  't',
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U16(Info.L2Len, 14);
  ASSERT_EQ_U16(Info.L3Offset, 14);
  ASSERT_EQ_U8(Info.L3Proto, VIRTIO_NET_HDR_OFFLOAD_L3_IPV4);
  ASSERT_EQ_U16(Info.L3Len, 20);
  ASSERT_EQ_U8(Info.L4Proto, 6);
  ASSERT_EQ_U16(Info.L4Offset, 34);
  ASSERT_EQ_U16(Info.L4Len, 20);
  ASSERT_EQ_U16(Info.PayloadOffset, 54);
  ASSERT_EQ_U16(Info.CsumStart, 34);
  ASSERT_EQ_U16(Info.CsumOffset, 16);
  ASSERT_EQ_U8(Info.IsFragmented, 0);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.NeedsCsum = 1;
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Hdr.Flags, VIRTIO_NET_HDR_F_NEEDS_CSUM);
  ASSERT_EQ_U8(Hdr.GsoType, VIRTIO_NET_HDR_GSO_NONE);
  ASSERT_EQ_U16(Hdr.HdrLen, 0);
  ASSERT_EQ_U16(Hdr.CsumStart, 34);
  ASSERT_EQ_U16(Hdr.CsumOffset, 16);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.Tso = 1;
  TxReq.TsoMss = 1460;
  St = VirtioNetHdrOffloadBuildTxHdrFromFrame(Frame, sizeof(Frame), &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Hdr.Flags, VIRTIO_NET_HDR_F_NEEDS_CSUM);
  ASSERT_EQ_U8(Hdr.GsoType, VIRTIO_NET_HDR_GSO_TCPV4);
  ASSERT_EQ_U16(Hdr.GsoSize, 1460);
  ASSERT_EQ_U16(Hdr.HdrLen, 54);
  ASSERT_EQ_U16(Hdr.CsumStart, 34);
  ASSERT_EQ_U16(Hdr.CsumOffset, 16);

  return 0;
}

static int test_no_offload_builds_zero(void) {
  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  /* Build-from-frame should not require a frame when no offload is requested. */
  memset(&TxReq, 0, sizeof(TxReq));
  memset(&Hdr, 0xAA, sizeof(Hdr));
  St = VirtioNetHdrOffloadBuildTxHdrFromFrame(NULL, 0, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Hdr.Flags, 0);
  ASSERT_EQ_U8(Hdr.GsoType, 0);
  ASSERT_EQ_U16(Hdr.HdrLen, 0);
  ASSERT_EQ_U16(Hdr.GsoSize, 0);
  ASSERT_EQ_U16(Hdr.CsumStart, 0);
  ASSERT_EQ_U16(Hdr.CsumOffset, 0);

  /* Build-from-info should also produce all zeros when no offload is requested. */
  memset(&Info, 0xCC, sizeof(Info));
  memset(&TxReq, 0, sizeof(TxReq));
  memset(&Hdr, 0xBB, sizeof(Hdr));
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Hdr.Flags, 0);
  ASSERT_EQ_U8(Hdr.GsoType, 0);
  ASSERT_EQ_U16(Hdr.HdrLen, 0);
  ASSERT_EQ_U16(Hdr.GsoSize, 0);
  ASSERT_EQ_U16(Hdr.CsumStart, 0);
  ASSERT_EQ_U16(Hdr.CsumOffset, 0);

  return 0;
}

static int test_ipv4_udp_no_vlan(void) {
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype IPv4 */
      0x08, 0x00,
      /* IPv4 header */
      0x45, 0x00, 0x00, 0x20, /* total_len=32 */
      0x00, 0x00, 0x00, 0x00, 0x40, 0x11, 0x00, 0x00, /* proto=UDP */
      0xc0, 0x00, 0x02, 0x01, 0xc6, 0x33, 0x64, 0x02,
      /* UDP header */
      0x04, 0xd2, 0x16, 0x2e, /* ports 1234->5678 */
      0x00, 0x0c, 0x00, 0x00, /* len=12, csum=0 */
      /* payload */
      'd',  'a',  't',  'a',
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U8(Info.L3Proto, VIRTIO_NET_HDR_OFFLOAD_L3_IPV4);
  ASSERT_EQ_U8(Info.L4Proto, 17);
  ASSERT_EQ_U16(Info.L4Offset, 34);
  ASSERT_EQ_U16(Info.L4Len, 8);
  ASSERT_EQ_U16(Info.PayloadOffset, 42);
  ASSERT_EQ_U16(Info.CsumStart, 34);
  ASSERT_EQ_U16(Info.CsumOffset, 6);
  ASSERT_EQ_U8(Info.IsFragmented, 0);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.NeedsCsum = 1;
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Hdr.Flags, VIRTIO_NET_HDR_F_NEEDS_CSUM);
  ASSERT_EQ_U8(Hdr.GsoType, VIRTIO_NET_HDR_GSO_NONE);
  ASSERT_EQ_U16(Hdr.HdrLen, 0);
  ASSERT_EQ_U16(Hdr.CsumStart, 34);
  ASSERT_EQ_U16(Hdr.CsumOffset, 6);

  /* TSO over UDP is unsupported */
  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.Tso = 1;
  TxReq.TsoMss = 1200;
  St = VirtioNetHdrOffloadBuildTxHdrFromFrame(Frame, sizeof(Frame), &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED);

  return 0;
}

static int test_ipv6_tcp_no_vlan(void) {
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype IPv6 */
      0x86, 0xdd,
      /* IPv6 header: version=6, payload_len=24, next=TCP, hop=64 */
      0x60, 0x00, 0x00, 0x00, 0x00, 0x18, 0x06, 0x40,
      /* src addr */
      0x20, 0x01, 0x0d, 0xb8, 0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    1,
      /* dst addr */
      0x20, 0x01, 0x0d, 0xb8, 0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    2,
      /* TCP header */
      0x1f, 0x90, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x50, 0x10, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00,
      /* payload */
      0x01, 0x02, 0x03, 0x04,
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U8(Info.L3Proto, VIRTIO_NET_HDR_OFFLOAD_L3_IPV6);
  ASSERT_EQ_U16(Info.L3Offset, 14);
  ASSERT_EQ_U16(Info.L3Len, 40);
  ASSERT_EQ_U8(Info.L4Proto, 6);
  ASSERT_EQ_U16(Info.L4Offset, 54);
  ASSERT_EQ_U16(Info.L4Len, 20);
  ASSERT_EQ_U16(Info.PayloadOffset, 74);
  ASSERT_EQ_U16(Info.CsumStart, 54);
  ASSERT_EQ_U16(Info.CsumOffset, 16);
  ASSERT_EQ_U8(Info.IsFragmented, 0);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.Tso = 1;
  TxReq.TsoMss = 1440;
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Hdr.Flags, VIRTIO_NET_HDR_F_NEEDS_CSUM);
  ASSERT_EQ_U8(Hdr.GsoType, VIRTIO_NET_HDR_GSO_TCPV6);
  ASSERT_EQ_U16(Hdr.GsoSize, 1440);
  ASSERT_EQ_U16(Hdr.HdrLen, 74);
  ASSERT_EQ_U16(Hdr.CsumStart, 54);
  ASSERT_EQ_U16(Hdr.CsumOffset, 16);

  return 0;
}

static int test_ipv6_hopbyhop_tcp(void) {
  /* Ethernet + IPv6 + hop-by-hop + TCP + 4-byte payload */
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype IPv6 */
      0x86, 0xdd,
      /* IPv6 header: version=6, payload_len=32, next=Hop-by-Hop(0), hop=64 */
      0x60, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x40,
      /* src addr */
      0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    1,
      /* dst addr */
      0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    2,
      /* Hop-by-Hop ext header: next=TCP, hdr_ext_len=0 (8 bytes total) */
      0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
      /* TCP header */
      0x1f, 0x90, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x50, 0x10, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00,
      /* payload */
      0x01, 0x02, 0x03, 0x04,
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U8(Info.L3Proto, VIRTIO_NET_HDR_OFFLOAD_L3_IPV6);
  ASSERT_EQ_U16(Info.L3Offset, 14);
  ASSERT_EQ_U16(Info.L3Len, 48);
  ASSERT_EQ_U8(Info.L4Proto, 6);
  ASSERT_EQ_U16(Info.L4Offset, 62);
  ASSERT_EQ_U16(Info.L4Len, 20);
  ASSERT_EQ_U16(Info.PayloadOffset, 82);
  ASSERT_EQ_U16(Info.CsumStart, 62);
  ASSERT_EQ_U16(Info.CsumOffset, 16);
  ASSERT_EQ_U8(Info.IsFragmented, 0);

  return 0;
}

static int test_vlan_tagged_ipv4_tcp(void) {
  /* Single 802.1Q VLAN tag */
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype VLAN */
      0x81, 0x00,
      /* VLAN tag: TCI + inner ethertype IPv4 */
      0x00, 0x01, 0x08, 0x00,
      /* IPv4 header (same as test_ipv4_tcp_no_vlan) */
      0x45, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x40, 0x00, 0x40, 0x06, 0x00, 0x00, 0xc0, 0x00, 0x02, 0x01,
      0xc6, 0x33, 0x64, 0x02,
      /* TCP header */
      0x1f, 0x90, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x50, 0x02, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00,
      /* payload */
      't',  'e',  's',  't',
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U16(Info.L2Len, 18);
  ASSERT_EQ_U16(Info.L3Offset, 18);
  ASSERT_EQ_U16(Info.L4Offset, 38);
  ASSERT_EQ_U16(Info.PayloadOffset, 58);
  ASSERT_EQ_U16(Info.CsumStart, 38);
  ASSERT_EQ_U16(Info.CsumOffset, 16);
  ASSERT_EQ_U8(Info.IsFragmented, 0);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.Tso = 1;
  TxReq.TsoMss = 1400;
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Hdr.GsoType, VIRTIO_NET_HDR_GSO_TCPV4);
  ASSERT_EQ_U16(Hdr.HdrLen, 58);
  ASSERT_EQ_U16(Hdr.CsumStart, 38);
  ASSERT_EQ_U16(Hdr.CsumOffset, 16);

  return 0;
}

static int test_qinq_tagged_ipv4_tcp(void) {
  /* QinQ: outer 0x88A8 + inner 0x8100 */
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype QinQ */
      0x88, 0xa8,
      /* outer tag */
      0x00, 0x01, 0x81, 0x00,
      /* inner tag */
      0x00, 0x02, 0x08, 0x00,
      /* IPv4 header (same as test_ipv4_tcp_no_vlan) */
      0x45, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x40, 0x00, 0x40, 0x06, 0x00, 0x00, 0xc0, 0x00, 0x02, 0x01,
      0xc6, 0x33, 0x64, 0x02,
      /* TCP header */
      0x1f, 0x90, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x50, 0x02, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00,
      /* payload */
      't',  'e',  's',  't',
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U16(Info.L2Len, 22);
  ASSERT_EQ_U16(Info.L3Offset, 22);
  ASSERT_EQ_U16(Info.L4Offset, 42);
  ASSERT_EQ_U16(Info.PayloadOffset, 62);
  ASSERT_EQ_U16(Info.CsumStart, 42);
  ASSERT_EQ_U16(Info.CsumOffset, 16);

  return 0;
}

static int test_malformed_and_truncated(void) {
  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  /* Too short for Ethernet header */
  {
    static const uint8_t Frame[] = {0};
    St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
    ASSERT_TRUE(St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  }

  /* VLAN ethertype but truncated tag */
  {
    static const uint8_t Frame[] = {
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0x81, 0x00,
        0x00, 0x01,
    };
    St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
    ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED);
  }

  /* IPv4 header with IHL claiming 24 bytes but truncated */
  {
    static const uint8_t Frame[] = {
        0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0x08, 0x00,
        0x46, 0x00, 0x00, 0x28, /* IHL=6 => 24 bytes, total_len=40 */
        0x00, 0x00, 0x00, 0x00, 0x40, 0x06, 0x00, 0x00,
        0,    0,    0,    0,    0,    0,    0,    0,
        /* only 20 bytes of IPv4 header present (missing options) */
    };
    St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
    ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED);
  }

  /* IPv6 header with payload_len exceeding available bytes */
  {
    static const uint8_t Frame[] = {
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x86, 0xdd,
        0x60, 0, 0, 0, 0x00, 0x10, 0x06, 0x40, /* payload_len=16, next=TCP */
        /* rest of IPv6 header truncated */
    };
    St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
    ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED);
  }

  return 0;
}

static int test_ipv4_tcp_options_boundary(void) {
  /* IPv4 IHL=6 (24 bytes), TCP data offset=7 (28 bytes) */
  static const uint8_t Frame[] = {
      /* dst/src */
      0,    1,    2,    3,    4,    5,    6,    7,    8,    9,    10,   11,
      /* ethertype IPv4 */
      0x08, 0x00,
      /* IPv4 header */
      0x46, 0x00, 0x00, 0x38, /* ihl=6, total_len=56 */
      0x00, 0x00, 0x00, 0x00, 0x40, 0x06, 0x00, 0x00,
      1,    2,    3,    4,    5,    6,    7,    8,
      /* 4 bytes of IPv4 options to make header 24 bytes */
      0xde, 0xad, 0xbe, 0xef,
      /* TCP header: 28 bytes */
      0x1f, 0x90, 0x00, 0x50, 0,    0,    0,    0,    0,    0,    0,    0,
      0x70, 0x10, 0,    0,    /* doff=7 => 28 bytes */
      0,    0,    0,    0,
      /* 8 bytes of TCP options */
      1,    1,    1,    1,    2,    2,    2,    2,
      /* payload: 4 bytes */
      0xaa, 0xbb, 0xcc, 0xdd,
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U16(Info.L2Len, 14);
  ASSERT_EQ_U16(Info.L3Len, 24);
  ASSERT_EQ_U16(Info.L4Offset, 38);
  ASSERT_EQ_U16(Info.L4Len, 28);
  ASSERT_EQ_U16(Info.PayloadOffset, 66);
  ASSERT_EQ_U16(Info.CsumStart, 38);
  ASSERT_EQ_U16(Info.CsumOffset, 16);
  ASSERT_EQ_U8(Info.IsFragmented, 0);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.Tso = 1;
  TxReq.TsoMss = 1200;
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U16(Hdr.HdrLen, 66);
  ASSERT_EQ_U16(Hdr.CsumStart, 38);
  ASSERT_EQ_U16(Hdr.CsumOffset, 16);

  return 0;
}

static int test_ipv4_icmp_parse(void) {
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype IPv4 */
      0x08, 0x00,
      /* IPv4 header (proto=ICMP) */
      0x45, 0x00, 0x00, 0x1c, /* total_len=28 */
      0x00, 0x00, 0x00, 0x00, 0x40, 0x01, 0x00, 0x00, /* proto=1 */
      0xc0, 0x00, 0x02, 0x01, 0xc6, 0x33, 0x64, 0x02,
      /* ICMP header (8 bytes) */
      0x08, 0x00, 0x00, 0x00, 0x12, 0x34, 0x00, 0x01,
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);

  ASSERT_EQ_U8(Info.L3Proto, VIRTIO_NET_HDR_OFFLOAD_L3_IPV4);
  ASSERT_EQ_U8(Info.L4Proto, 1);
  ASSERT_EQ_U16(Info.L4Offset, 34);
  ASSERT_EQ_U16(Info.L4Len, 0);
  ASSERT_EQ_U16(Info.PayloadOffset, 34);
  ASSERT_EQ_U8(Info.IsFragmented, 0);

  /* Checksum offload requires TCP/UDP. */
  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.NeedsCsum = 1;
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED);

  return 0;
}

static int test_ipv4_fragmented_tcp_rejected(void) {
  /* Ethernet + IPv4 + TCP */
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype IPv4 */
      0x08, 0x00,
      /* IPv4 header */
      0x45, 0x00, 0x00, 0x2c, /* v4 ihl=5, total_len=44 */
      0x00, 0x00, 0x20, 0x00, /* flags: MF set */
      0x40, 0x06, 0x00, 0x00, /* ttl=64, proto=TCP */
      0xc0, 0x00, 0x02, 0x01, /* src */
      0xc6, 0x33, 0x64, 0x02, /* dst */
      /* TCP header */
      0x1f, 0x90, 0x00, 0x50, /* ports */
      0x00, 0x00, 0x00, 0x00, /* seq */
      0x00, 0x00, 0x00, 0x00, /* ack */
      0x50, 0x02, 0x00, 0x00, /* doff=5 */
      0x00, 0x00, 0x00, 0x00, /* csum, urg */
      /* payload */
      't',  'e',  's',  't',
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Info.IsFragmented, 1);
  ASSERT_EQ_U8(Info.L4Proto, 6);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.NeedsCsum = 1;
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.Tso = 1;
  TxReq.TsoMss = 1460;
  St = VirtioNetHdrOffloadBuildTxHdrFromFrame(Frame, sizeof(Frame), &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED);

  return 0;
}

static int test_ipv6_fragmented_tcp_rejected(void) {
  /* Ethernet + IPv6 + Fragment + TCP */
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype IPv6 */
      0x86, 0xdd,
      /* IPv6 header: version=6, payload_len=32, next=Fragment(44), hop=64 */
      0x60, 0x00, 0x00, 0x00, 0x00, 0x20, 0x2c, 0x40,
      /* src addr */
      0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    1,
      /* dst addr */
      0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    2,
      /* Fragment header: next=TCP, reserved=0, off=0, M=1 */
      0x06, 0x00, 0x00, 0x01, 0x12, 0x34, 0x56, 0x78,
      /* TCP header */
      0x1f, 0x90, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x50, 0x10, 0x00, 0x00,
      0x00, 0x00, 0x00, 0x00,
      /* payload */
      0x01, 0x02, 0x03, 0x04,
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST TxReq;
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Info.L3Proto, VIRTIO_NET_HDR_OFFLOAD_L3_IPV6);
  ASSERT_EQ_U8(Info.IsFragmented, 1);
  ASSERT_EQ_U8(Info.L4Proto, 6);

  memset(&TxReq, 0, sizeof(TxReq));
  TxReq.NeedsCsum = 1;
  St = VirtioNetHdrOffloadBuildTxHdr(&Info, &TxReq, &Hdr);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED);

  return 0;
}

static int test_ipv6_nonfirst_fragment_parse_ok(void) {
  /* Ethernet + IPv6 + Fragment(offset!=0) + 4 bytes payload; no TCP header present. */
  static const uint8_t Frame[] = {
      /* dst/src */
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
      /* ethertype IPv6 */
      0x86, 0xdd,
      /* IPv6 header: version=6, payload_len=12, next=Fragment(44), hop=64 */
      0x60, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x2c, 0x40,
      /* src addr */
      0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    1,
      /* dst addr */
      0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    0,    2,
      /* Fragment header: next=TCP, offset=1 (8 bytes), M=0 */
      0x06, 0x00, 0x00, 0x08, 0x12, 0x34, 0x56, 0x78,
      /* fragment payload */
      0xde, 0xad, 0xbe, 0xef,
  };

  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  St = VirtioNetHdrOffloadParseFrame(Frame, sizeof(Frame), &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Info.L3Proto, VIRTIO_NET_HDR_OFFLOAD_L3_IPV6);
  ASSERT_EQ_U8(Info.IsFragmented, 1);
  ASSERT_EQ_U8(Info.L4Proto, 6);
  ASSERT_EQ_U16(Info.L3Len, 48);
  ASSERT_EQ_U16(Info.L4Len, 0);
  ASSERT_EQ_U16(Info.PayloadOffset, 62);

  return 0;
}

static int test_rx_hdr_parse(void) {
  VIRTIO_NET_HDR Hdr;
  VIRTIO_NET_HDR_OFFLOAD_RX_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  VirtioNetHdrOffloadZero(&Hdr);
  Hdr.Flags = VIRTIO_NET_HDR_F_DATA_VALID;
  Hdr.GsoType = VIRTIO_NET_HDR_GSO_NONE;
  Hdr.HdrLen = 54;
  St = VirtioNetHdrOffloadParseRxHdr(&Hdr, &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Info.CsumValid, 1);
  ASSERT_EQ_U8(Info.NeedsCsum, 0);
  ASSERT_EQ_U8(Info.IsGso, 0);
  ASSERT_EQ_U16(Info.HdrLen, 54);

  VirtioNetHdrOffloadZero(&Hdr);
  Hdr.Flags = (uint8_t)(VIRTIO_NET_HDR_F_NEEDS_CSUM | VIRTIO_NET_HDR_F_DATA_VALID);
  Hdr.GsoType = (uint8_t)(VIRTIO_NET_HDR_GSO_TCPV4 | VIRTIO_NET_HDR_GSO_ECN);
  Hdr.GsoSize = 1460;
  St = VirtioNetHdrOffloadParseRxHdr(&Hdr, &Info);
  ASSERT_EQ_INT(St, VIRTIO_NET_HDR_OFFLOAD_STATUS_OK);
  ASSERT_EQ_U8(Info.NeedsCsum, 1);
  ASSERT_EQ_U8(Info.CsumValid, 1);
  ASSERT_EQ_U8(Info.IsGso, 1);
  ASSERT_EQ_U8(Info.GsoType, VIRTIO_NET_HDR_GSO_TCPV4);
  ASSERT_EQ_U8(Info.GsoEcn, 1);
  ASSERT_EQ_U16(Info.GsoSize, 1460);

  return 0;
}

int main(void) {
  int rc;

  rc = 0;
  rc |= test_ipv4_tcp_no_vlan();
  rc |= test_no_offload_builds_zero();
  rc |= test_ipv4_udp_no_vlan();
  rc |= test_ipv6_tcp_no_vlan();
  rc |= test_ipv6_hopbyhop_tcp();
  rc |= test_vlan_tagged_ipv4_tcp();
  rc |= test_qinq_tagged_ipv4_tcp();
  rc |= test_malformed_and_truncated();
  rc |= test_ipv4_tcp_options_boundary();
  rc |= test_ipv4_icmp_parse();
  rc |= test_ipv4_fragmented_tcp_rejected();
  rc |= test_ipv6_fragmented_tcp_rejected();
  rc |= test_ipv6_nonfirst_fragment_parse_ok();
  rc |= test_rx_hdr_parse();

  if (rc == 0) {
    printf("virtio_net_hdr_offload_tests: PASS\n");
  }
  return rc;
}
