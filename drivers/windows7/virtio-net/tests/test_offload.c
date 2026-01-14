/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "aero_virtio_net_offload.h"

/*
 * Keep assert() active even in Release builds (CMake defines NDEBUG).
 * See drivers/windows7/virtio/common/tests/test_main.c for rationale.
 */
#undef assert
#define assert(expr)                                                                                                   \
    do {                                                                                                               \
        if (!(expr)) {                                                                                                 \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                \
            abort();                                                                                                   \
        }                                                                                                              \
    } while (0)

static void build_eth(uint8_t *dst, uint16_t ethertype)
{
    /* dst/src mac */
    memset(dst, 0x11, 6);
    memset(dst + 6, 0x22, 6);
    dst[12] = (uint8_t)(ethertype >> 8);
    dst[13] = (uint8_t)(ethertype & 0xff);
}

static size_t build_eth_vlan(uint8_t *dst, uint16_t inner_ethertype)
{
    /* 802.1Q tag: ethertype 0x8100 + TCI + inner ethertype */
    memset(dst, 0x11, 6);
    memset(dst + 6, 0x22, 6);
    dst[12] = 0x81;
    dst[13] = 0x00;
    dst[14] = 0x00; /* TCI */
    dst[15] = 0x00;
    dst[16] = (uint8_t)(inner_ethertype >> 8);
    dst[17] = (uint8_t)(inner_ethertype & 0xff);
    return 18;
}

static size_t build_eth_qinq(uint8_t *dst, uint16_t inner_ethertype)
{
    /* QinQ: outer 0x88A8 + TCI + inner 0x8100 + TCI + inner ethertype */
    memset(dst, 0x11, 6);
    memset(dst + 6, 0x22, 6);
    dst[12] = 0x88;
    dst[13] = 0xA8;
    dst[14] = 0x00; /* outer TCI */
    dst[15] = 0x00;
    dst[16] = 0x81;
    dst[17] = 0x00;
    dst[18] = 0x00; /* inner TCI */
    dst[19] = 0x00;
    dst[20] = (uint8_t)(inner_ethertype >> 8);
    dst[21] = (uint8_t)(inner_ethertype & 0xff);
    return 22;
}

static void build_ipv4_tcp(uint8_t *dst, size_t payload_len)
{
    /* IPv4 header */
    const uint16_t total_len = (uint16_t)(20 + 20 + payload_len);
    memset(dst, 0, 20);
    dst[0] = (4u << 4) | 5u;
    dst[2] = (uint8_t)(total_len >> 8);
    dst[3] = (uint8_t)(total_len & 0xff);
    dst[8] = 64;
    dst[9] = 6; /* TCP */
    /* src/dst */
    dst[12] = 192;
    dst[13] = 0;
    dst[14] = 2;
    dst[15] = 1;
    dst[16] = 198;
    dst[17] = 51;
    dst[18] = 100;
    dst[19] = 2;
}

static void build_ipv4_udp(uint8_t *dst, size_t payload_len)
{
    /* IPv4 header */
    const uint16_t total_len = (uint16_t)(20 + 8 + payload_len);
    memset(dst, 0, 20);
    dst[0] = (4u << 4) | 5u;
    dst[2] = (uint8_t)(total_len >> 8);
    dst[3] = (uint8_t)(total_len & 0xff);
    dst[8] = 64;
    dst[9] = 17; /* UDP */
    /* src/dst */
    dst[12] = 192;
    dst[13] = 0;
    dst[14] = 2;
    dst[15] = 1;
    dst[16] = 198;
    dst[17] = 51;
    dst[18] = 100;
    dst[19] = 2;
}

static void build_ipv4_tcp_with_ihl(uint8_t *dst, size_t payload_len, uint8_t ihl_words, uint16_t tcp_header_bytes)
{
    const uint16_t ip_header_bytes = (uint16_t)ihl_words * 4u;
    const uint16_t total_len = (uint16_t)(ip_header_bytes + tcp_header_bytes + payload_len);
    memset(dst, 0, ip_header_bytes);
    dst[0] = (4u << 4) | (ihl_words & 0x0f);
    dst[2] = (uint8_t)(total_len >> 8);
    dst[3] = (uint8_t)(total_len & 0xff);
    dst[8] = 64;
    dst[9] = 6; /* TCP */
    /* src/dst */
    dst[12] = 192;
    dst[13] = 0;
    dst[14] = 2;
    dst[15] = 1;
    dst[16] = 198;
    dst[17] = 51;
    dst[18] = 100;
    dst[19] = 2;
}

static void build_ipv6_tcp(uint8_t *dst, size_t payload_len)
{
    /* IPv6 header */
    const uint16_t payload = (uint16_t)(20 + payload_len); /* TCP header + payload */
    memset(dst, 0, 40);
    dst[0] = (6u << 4);
    dst[4] = (uint8_t)(payload >> 8);
    dst[5] = (uint8_t)(payload & 0xff);
    dst[6] = 6;  /* TCP */
    dst[7] = 64; /* hop limit */
    /* src/dst addresses left as zero */
}

static void build_ipv6_udp(uint8_t *dst, size_t payload_len)
{
    /* IPv6 header */
    const uint16_t payload = (uint16_t)(8 + payload_len); /* UDP header + payload */
    memset(dst, 0, 40);
    dst[0] = (6u << 4);
    dst[4] = (uint8_t)(payload >> 8);
    dst[5] = (uint8_t)(payload & 0xff);
    dst[6] = 17; /* UDP */
    dst[7] = 64; /* hop limit */
    /* src/dst addresses left as zero */
}

static void build_ipv6_hopbyhop_tcp(uint8_t *dst, size_t payload_len)
{
    /* IPv6 base header with Hop-by-Hop extension header before TCP. */
    const uint16_t payload = (uint16_t)(8 + 20 + payload_len); /* ext + TCP header + payload */
    memset(dst, 0, 40);
    dst[0] = (6u << 4);
    dst[4] = (uint8_t)(payload >> 8);
    dst[5] = (uint8_t)(payload & 0xff);
    dst[6] = 0;  /* Hop-by-Hop */
    dst[7] = 64; /* hop limit */

    /* Hop-by-Hop extension header: NextHeader=TCP, HdrExtLen=0 (8 bytes total). */
    memset(dst + 40, 0, 8);
    dst[40] = 6;  /* next = TCP */
    dst[41] = 0;  /* 8 bytes */
}

static void build_ipv6_hopbyhop_udp(uint8_t *dst, size_t payload_len)
{
    /* IPv6 base header with Hop-by-Hop extension header before UDP. */
    const uint16_t payload = (uint16_t)(8 + 8 + payload_len); /* ext + UDP header + payload */
    memset(dst, 0, 40);
    dst[0] = (6u << 4);
    dst[4] = (uint8_t)(payload >> 8);
    dst[5] = (uint8_t)(payload & 0xff);
    dst[6] = 0;  /* Hop-by-Hop */
    dst[7] = 64; /* hop limit */

    /* Hop-by-Hop extension header: NextHeader=UDP, HdrExtLen=0 (8 bytes total). */
    memset(dst + 40, 0, 8);
    dst[40] = 17; /* next = UDP */
    dst[41] = 0;  /* 8 bytes */
}

static void build_tcp_header(uint8_t *dst)
{
    memset(dst, 0, 20);
    /* data offset = 5 (20 bytes) */
    dst[12] = 5u << 4;
}

static void build_udp_header(uint8_t *dst)
{
    /* Minimal UDP header (8 bytes). */
    memset(dst, 0, 8);
}

static void build_tcp_header_with_data_offset(uint8_t *dst, uint8_t data_offset_words)
{
    const size_t hdr_bytes = (size_t)data_offset_words * 4u;
    memset(dst, 0, hdr_bytes);
    dst[12] = (uint8_t)(data_offset_words << 4);
}

static void test_ipv4_tcp_checksum_only(void)
{
    uint8_t pkt[14 + 20 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 0);
    build_tcp_header(pkt + 14 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.HdrLen == 0);
    assert(hdr.GsoSize == 0);
    assert(hdr.CsumStart == (uint16_t)(14 + 20));
    assert(hdr.CsumOffset == 16);
    assert(info.IpVersion == 4);
    assert(info.L4Protocol == 6);
}

static void test_ipv4_udp_checksum_only(void)
{
    uint8_t pkt[14 + 20 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_udp(pkt + 14, 0);
    build_udp_header(pkt + 14 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.HdrLen == 0);
    assert(hdr.GsoSize == 0);
    assert(hdr.CsumStart == (uint16_t)(14 + 20));
    assert(hdr.CsumOffset == 6);
    assert(info.IpVersion == 4);
    assert(info.L4Protocol == 17);
}

static void test_ipv4_tcp_udp_checksum_intent_invalid(void)
{
    uint8_t pkt[14 + 20 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 0);
    build_tcp_header(pkt + 14 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_INVAL);
}

static void test_ipv4_tcp_udp_checksum_only_rejected(void)
{
    uint8_t pkt[14 + 20 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 0);
    build_tcp_header(pkt + 14 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_UNSUPPORTED_L4_PROTOCOL);
}

static void test_udp_intent_with_tso_invalid(void)
{
    uint8_t pkt[14 + 20 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_udp(pkt + 14, 0);
    build_udp_header(pkt + 14 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;
    intent.WantTso = 1;
    intent.TsoMss = 1200;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_INVAL);
}

static void test_no_offload(void)
{
    uint8_t pkt[14 + 20 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;
    AEROVNET_VIRTIO_NET_HDR zero;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 0);
    build_tcp_header(pkt + 14 + 20);

    memset(&intent, 0, sizeof(intent));
    memset(&hdr, 0xA5, sizeof(hdr));
    memset(&zero, 0, sizeof(zero));

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(memcmp(&hdr, &zero, sizeof(hdr)) == 0);
}

static void test_ipv6_tcp_checksum_only(void)
{
    uint8_t pkt[14 + 40 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);
    build_ipv6_tcp(pkt + 14, 0);
    build_tcp_header(pkt + 14 + 40);

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.HdrLen == 0);
    assert(hdr.GsoSize == 0);
    assert(hdr.CsumStart == (uint16_t)(14 + 40));
    assert(hdr.CsumOffset == 16);
    assert(info.IpVersion == 6);
    assert(info.L4Protocol == 6);
}

static void test_ipv6_udp_checksum_only(void)
{
    uint8_t pkt[14 + 40 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);
    build_ipv6_udp(pkt + 14, 0);
    build_udp_header(pkt + 14 + 40);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.HdrLen == 0);
    assert(hdr.GsoSize == 0);
    assert(hdr.CsumStart == (uint16_t)(14 + 40));
    assert(hdr.CsumOffset == 6);
    assert(info.IpVersion == 6);
    assert(info.L4Protocol == 17);
}

static void test_ipv6_hopbyhop_udp_checksum_only(void)
{
    uint8_t pkt[14 + 40 + 8 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);
    build_ipv6_hopbyhop_udp(pkt + 14, 0);
    build_udp_header(pkt + 14 + 40 + 8);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.CsumStart == (uint16_t)(14 + 40 + 8));
    assert(hdr.CsumOffset == 6);
    assert(info.IpVersion == 6);
    assert(info.L4Protocol == 17);
}

static void test_ipv6_vlan_tcp_checksum_only(void)
{
    uint8_t pkt[18 + 40 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth_vlan(pkt, 0x86DD);
    build_ipv6_tcp(pkt + 18, 0);
    build_tcp_header(pkt + 18 + 40);

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.HdrLen == 0);
    assert(hdr.GsoSize == 0);
    assert(hdr.CsumStart == (uint16_t)(18 + 40));
    assert(hdr.CsumOffset == 16);
    assert(info.IpVersion == 6);
    assert(info.L4Protocol == 6);
}

static void test_ipv6_vlan_udp_checksum_only(void)
{
    uint8_t pkt[18 + 40 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth_vlan(pkt, 0x86DD);
    build_ipv6_udp(pkt + 18, 0);
    build_udp_header(pkt + 18 + 40);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.HdrLen == 0);
    assert(hdr.GsoSize == 0);
    assert(hdr.CsumStart == (uint16_t)(18 + 40));
    assert(hdr.CsumOffset == 6);
    assert(info.IpVersion == 6);
    assert(info.L4Protocol == 17);
}

static void test_ipv4_vlan_tcp_checksum_only(void)
{
    uint8_t pkt[18 + 20 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth_vlan(pkt, 0x0800);
    build_ipv4_tcp(pkt + 18, 0);
    build_tcp_header(pkt + 18 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.CsumStart == (uint16_t)(18 + 20));
    assert(hdr.CsumOffset == 16);
}

static void test_ipv4_vlan_udp_checksum_only(void)
{
    uint8_t pkt[18 + 20 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth_vlan(pkt, 0x0800);
    build_ipv4_udp(pkt + 18, 0);
    build_udp_header(pkt + 18 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.CsumStart == (uint16_t)(18 + 20));
    assert(hdr.CsumOffset == 6);
}

static void test_ipv4_qinq_udp_checksum_only(void)
{
    uint8_t pkt[22 + 20 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth_qinq(pkt, 0x0800);
    build_ipv4_udp(pkt + 22, 0);
    build_udp_header(pkt + 22 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.CsumStart == (uint16_t)(22 + 20));
    assert(hdr.CsumOffset == 6);
    assert(info.IpVersion == 4);
    assert(info.L4Protocol == 17);
}

static void test_ipv4_ip_options_tcp_checksum_only(void)
{
    /* IHL=6 (24 bytes). */
    uint8_t pkt[14 + 24 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp_with_ihl(pkt + 14, 0, 6, 20);
    build_tcp_header(pkt + 14 + 24);

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.CsumStart == (uint16_t)(14 + 24));
    assert(hdr.CsumOffset == 16);
}

static void test_ipv4_ip_options_tcp_lso(void)
{
    /* IPv4 IHL=6 (24 bytes). */
    uint8_t pkt[14 + 24 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp_with_ihl(pkt + 14, 4000, 6, 20);
    build_tcp_header(pkt + 14 + 24);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1460;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4);
    assert(hdr.HdrLen == (uint16_t)(14 + 24 + 20));
    assert(hdr.GsoSize == 1460);
    assert(hdr.CsumStart == (uint16_t)(14 + 24));
    assert(hdr.CsumOffset == 16);
}

static void test_ipv4_tcp_lso(void)
{
    uint8_t pkt[14 + 20 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1460;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4);
    assert(hdr.HdrLen == (uint16_t)(14 + 20 + 20));
    assert(hdr.GsoSize == 1460);
    assert(hdr.CsumStart == (uint16_t)(14 + 20));
    assert(hdr.CsumOffset == 16);
    assert(info.IpVersion == 4);
}

static void test_ipv4_tcp_lso_partial_headers(void)
{
    /* Only Ethernet+IPv4+TCP headers are present, but total_len claims a larger packet. */
    uint8_t pkt[14 + 20 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1460;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4);
    assert(hdr.HdrLen == (uint16_t)(14 + 20 + 20));
    assert(hdr.GsoSize == 1460);
    assert(hdr.CsumStart == (uint16_t)(14 + 20));
    assert(hdr.CsumOffset == 16);
    assert(info.HeadersLen == (uint16_t)(14 + 20 + 20));
}

static void test_ipv4_tcp_lso_ecn_when_cwr_and_enabled(void)
{
    uint8_t pkt[14 + 20 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 20);
    /* TCP flags (byte 13): set CWR. */
    pkt[14 + 20 + 13] = 0x80;

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoEcn = 1;
    intent.TsoMss = 1460;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.GsoType == (AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4 | AEROVNET_VIRTIO_NET_HDR_GSO_ECN));
}

static void test_ipv4_tcp_lso_no_ecn_when_enabled_but_no_cwr(void)
{
    uint8_t pkt[14 + 20 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 20);
    /* TCP flags remain 0 (no CWR). */

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoEcn = 1;
    intent.TsoMss = 1460;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4);
}

static void test_ipv4_tcp_lso_no_ecn_when_cwr_but_disabled(void)
{
    uint8_t pkt[14 + 20 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 20);
    /* TCP flags: CWR set. */
    pkt[14 + 20 + 13] = 0x80;

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoEcn = 0;
    intent.TsoMss = 1460;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4);
}

static void test_ipv4_tcp_options_lso(void)
{
    /* TCP header data offset = 6 (24 bytes). */
    uint8_t pkt[14 + 20 + 24 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp_with_ihl(pkt + 14, 4000, 5, 24);
    build_tcp_header_with_data_offset(pkt + 14 + 20, 6);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1460;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4);
    assert(hdr.HdrLen == (uint16_t)(14 + 20 + 24));
    assert(hdr.GsoSize == 1460);
    assert(hdr.CsumStart == (uint16_t)(14 + 20));
    assert(hdr.CsumOffset == 16);
}

static void test_ipv4_qinq_tcp_lso(void)
{
    uint8_t pkt[22 + 20 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth_qinq(pkt, 0x0800);
    build_ipv4_tcp(pkt + 22, 4000);
    build_tcp_header(pkt + 22 + 20);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1400;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4);
    assert(hdr.HdrLen == (uint16_t)(22 + 20 + 20));
    assert(hdr.GsoSize == 1400);
    assert(hdr.CsumStart == (uint16_t)(22 + 20));
    assert(hdr.CsumOffset == 16);
}

static void test_ipv6_qinq_tcp_lso(void)
{
    uint8_t pkt[22 + 40 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth_qinq(pkt, 0x86DD);
    build_ipv6_tcp(pkt + 22, 4000);
    build_tcp_header(pkt + 22 + 40);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1440;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV6);
    assert(hdr.HdrLen == (uint16_t)(22 + 40 + 20));
    assert(hdr.GsoSize == 1440);
    assert(hdr.CsumStart == (uint16_t)(22 + 40));
    assert(hdr.CsumOffset == 16);
}

static void test_ipv6_tcp_lso(void)
{
    uint8_t pkt[14 + 40 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);
    build_ipv6_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 40);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1440;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV6);
    assert(hdr.HdrLen == (uint16_t)(14 + 40 + 20));
    assert(hdr.GsoSize == 1440);
    assert(hdr.CsumStart == (uint16_t)(14 + 40));
    assert(hdr.CsumOffset == 16);
    assert(info.IpVersion == 6);
}

static void test_ipv6_hopbyhop_tcp_lso(void)
{
    uint8_t pkt[14 + 40 + 8 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);
    build_ipv6_hopbyhop_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 40 + 8);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1440;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV6);
    assert(hdr.HdrLen == (uint16_t)(14 + 40 + 8 + 20));
    assert(hdr.GsoSize == 1440);
    assert(hdr.CsumStart == (uint16_t)(14 + 40 + 8));
    assert(hdr.CsumOffset == 16);
    assert(info.IpVersion == 6);
}

static void test_ipv6_tcp_lso_ecn_when_cwr_and_enabled(void)
{
    uint8_t pkt[14 + 40 + 20 + 4000];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);
    build_ipv6_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 40);
    /* TCP flags (byte 13): set CWR. */
    pkt[14 + 40 + 13] = 0x80;

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoEcn = 1;
    intent.TsoMss = 1440;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.GsoType == (AEROVNET_VIRTIO_NET_HDR_GSO_TCPV6 | AEROVNET_VIRTIO_NET_HDR_GSO_ECN));
}

static void test_ipv6_tcp_lso_partial_headers(void)
{
    /* Only Ethernet+IPv6+TCP headers are present, but payload_len claims a larger packet. */
    uint8_t pkt[14 + 40 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_PARSE_INFO info;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);
    build_ipv6_tcp(pkt + 14, 4000);
    build_tcp_header(pkt + 14 + 40);

    memset(&intent, 0, sizeof(intent));
    intent.WantTso = 1;
    intent.TsoMss = 1440;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, &info);
    assert(res == AEROVNET_OFFLOAD_OK);

    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_TCPV6);
    assert(hdr.HdrLen == (uint16_t)(14 + 40 + 20));
    assert(hdr.GsoSize == 1440);
    assert(hdr.CsumStart == (uint16_t)(14 + 40));
    assert(hdr.CsumOffset == 16);
    assert(info.HeadersLen == (uint16_t)(14 + 40 + 20));
}

static void test_ipv4_fragment_rejected(void)
{
    uint8_t pkt[14 + 20 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_tcp(pkt + 14, 0);
    build_tcp_header(pkt + 14 + 20);

    /* Set MF (more fragments) flag. */
    pkt[14 + 6] = 0x20;
    pkt[14 + 7] = 0x00;

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_UNSUPPORTED_FRAGMENTATION);
}

static void test_ipv4_fragment_udp_rejected(void)
{
    uint8_t pkt[14 + 20 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_udp(pkt + 14, 0);
    build_udp_header(pkt + 14 + 20);

    /* Set MF (more fragments) flag. */
    pkt[14 + 6] = 0x20;
    pkt[14 + 7] = 0x00;

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_UNSUPPORTED_FRAGMENTATION);
}

static void test_ipv6_fragment_rejected(void)
{
    uint8_t pkt[14 + 40 + 8 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);

    /* IPv6 header: NextHeader = Fragment (44) */
    memset(pkt + 14, 0, 40);
    pkt[14] = (6u << 4);
    pkt[14 + 4] = 0;
    pkt[14 + 5] = (uint8_t)(8 + 20); /* fragment header + TCP header */
    pkt[14 + 6] = 44;
    pkt[14 + 7] = 64;

    /* Fragment header: Next = TCP, rest zero */
    memset(pkt + 14 + 40, 0, 8);
    pkt[14 + 40] = 6;

    build_tcp_header(pkt + 14 + 40 + 8);

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_UNSUPPORTED_FRAGMENTATION);
}

static void test_ipv6_hopbyhop_tcp_checksum_only(void)
{
    uint8_t pkt[14 + 40 + 8 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x86DD);
    build_ipv6_hopbyhop_tcp(pkt + 14, 0);
    build_tcp_header(pkt + 14 + 40 + 8);

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_OK);
    assert(hdr.Flags == AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM);
    assert(hdr.GsoType == AEROVNET_VIRTIO_NET_HDR_GSO_NONE);
    assert(hdr.CsumStart == (uint16_t)(14 + 40 + 8));
    assert(hdr.CsumOffset == 16);
}

static void test_unsupported_protocol(void)
{
    uint8_t pkt[14 + 20 + 8];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    /* IPv4 header but with UDP protocol (17). */
    memset(pkt + 14, 0, 20);
    pkt[14] = (4u << 4) | 5u;
    pkt[14 + 9] = 17u;

    memset(&intent, 0, sizeof(intent));
    intent.WantTcpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_UNSUPPORTED_L4_PROTOCOL);
}

static void test_unsupported_ethertype(void)
{
    uint8_t pkt[14];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0806); /* ARP */

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_UNSUPPORTED_ETHERTYPE);
}

static void test_short_frame_rejected(void)
{
    uint8_t pkt[13];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    memset(pkt, 0, sizeof(pkt));

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT);
}

static void test_short_udp_header_rejected(void)
{
    uint8_t pkt[14 + 20];
    AEROVNET_TX_OFFLOAD_INTENT intent;
    AEROVNET_VIRTIO_NET_HDR hdr;
    AEROVNET_OFFLOAD_RESULT res;

    build_eth(pkt, 0x0800);
    build_ipv4_udp(pkt + 14, 0);

    memset(&intent, 0, sizeof(intent));
    intent.WantUdpChecksum = 1;

    res = AerovNetBuildTxVirtioNetHdr(pkt, sizeof(pkt), &intent, &hdr, NULL);
    assert(res == AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT);
}

int main(void)
{
    test_ipv4_tcp_checksum_only();
    test_ipv4_udp_checksum_only();
    test_ipv4_tcp_udp_checksum_intent_invalid();
    test_ipv4_tcp_udp_checksum_only_rejected();
    test_udp_intent_with_tso_invalid();
    test_no_offload();
    test_ipv6_tcp_checksum_only();
    test_ipv6_udp_checksum_only();
    test_ipv6_hopbyhop_udp_checksum_only();
    test_ipv6_vlan_tcp_checksum_only();
    test_ipv6_vlan_udp_checksum_only();
    test_ipv4_vlan_tcp_checksum_only();
    test_ipv4_vlan_udp_checksum_only();
    test_ipv4_qinq_udp_checksum_only();
    test_ipv4_ip_options_tcp_checksum_only();
    test_ipv4_ip_options_tcp_lso();
    test_ipv4_tcp_lso();
    test_ipv4_tcp_lso_partial_headers();
    test_ipv4_tcp_lso_ecn_when_cwr_and_enabled();
    test_ipv4_tcp_lso_no_ecn_when_enabled_but_no_cwr();
    test_ipv4_tcp_lso_no_ecn_when_cwr_but_disabled();
    test_ipv4_tcp_options_lso();
    test_ipv4_qinq_tcp_lso();
    test_ipv6_qinq_tcp_lso();
    test_ipv6_tcp_lso();
    test_ipv6_hopbyhop_tcp_lso();
    test_ipv6_tcp_lso_ecn_when_cwr_and_enabled();
    test_ipv6_tcp_lso_partial_headers();
    test_ipv4_fragment_rejected();
    test_ipv4_fragment_udp_rejected();
    test_ipv6_fragment_rejected();
    test_ipv6_hopbyhop_tcp_checksum_only();
    test_unsupported_protocol();
    test_unsupported_ethertype();
    test_short_frame_rejected();
    test_short_udp_header_rejected();
    return 0;
}
