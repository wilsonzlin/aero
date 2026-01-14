#pragma once

/*
 * Pure-C helper for building virtio-net transmit headers for checksum/GSO offloads.
 *
 * This header is intentionally free of NDIS/WDK types so it can be used by
 * host-side unit tests (Linux CI) as well as the Windows miniport.
 */

#include <stddef.h>

/*
 * MSVC before VS2010 lacks <stdint.h>. WDK 7.1 uses an older toolset.
 *
 * Multiple portable modules may be included into the same translation unit; use
 * a shared macro to avoid conflicting typedef redefinitions.
 */
#if defined(_MSC_VER) && _MSC_VER < 1600
#ifndef AERO_PORTABLE_STDINT_UINT_TYPES_DEFINED
#define AERO_PORTABLE_STDINT_UINT_TYPES_DEFINED 1
typedef unsigned __int8 uint8_t;
typedef unsigned __int16 uint16_t;
typedef unsigned __int32 uint32_t;
typedef unsigned __int64 uint64_t;
#endif
#else
#include <stdint.h>
#endif

#pragma pack(push, 1)
typedef struct _AEROVNET_VIRTIO_NET_HDR {
  uint8_t Flags;
  uint8_t GsoType;
  uint16_t HdrLen;
  uint16_t GsoSize;
  uint16_t CsumStart;
  uint16_t CsumOffset;
} AEROVNET_VIRTIO_NET_HDR;
#pragma pack(pop)

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
_Static_assert(sizeof(AEROVNET_VIRTIO_NET_HDR) == 10, "AEROVNET_VIRTIO_NET_HDR must match virtio-net base header size");
#endif

/* virtio-net header flags */
#define AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM 1u

/* virtio-net GSO types */
#define AEROVNET_VIRTIO_NET_HDR_GSO_NONE 0u
#define AEROVNET_VIRTIO_NET_HDR_GSO_TCPV4 1u
#define AEROVNET_VIRTIO_NET_HDR_GSO_TCPV6 4u
#define AEROVNET_VIRTIO_NET_HDR_GSO_ECN 0x80u

typedef struct _AEROVNET_TX_OFFLOAD_INTENT {
  /* Request TCP checksum offload (no segmentation). */
  uint8_t WantTcpChecksum;

  /* Request UDP checksum offload (no segmentation). */
  uint8_t WantUdpChecksum;

  /* Request TCP segmentation offload (TSO/LSO). Implies NEEDS_CSUM. */
  uint8_t WantTso;

  /* If set, set the virtio-net ECN bit when CWR is present (TSO only). */
  uint8_t TsoEcn;

  /* MSS for TSO/LSO (bytes of TCP payload per segment). */
  uint16_t TsoMss;
} AEROVNET_TX_OFFLOAD_INTENT;

typedef struct _AEROVNET_OFFLOAD_PARSE_INFO {
  uint8_t IpVersion; /* 4 or 6 when parsed successfully. */
  uint8_t L4Protocol; /* e.g. 6 for TCP, 17 for UDP. */

  uint16_t L2Len;
  uint16_t L3Len;
  uint16_t L4Len;
  uint16_t L4Offset;
  uint16_t HeadersLen;
} AEROVNET_OFFLOAD_PARSE_INFO;

typedef enum _AEROVNET_OFFLOAD_RESULT {
  AEROVNET_OFFLOAD_OK = 0,
  AEROVNET_OFFLOAD_ERR_INVAL = 1,
  AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT = 2,
  AEROVNET_OFFLOAD_ERR_UNSUPPORTED_ETHERTYPE = 3,
  AEROVNET_OFFLOAD_ERR_UNSUPPORTED_IP_VERSION = 4,
  AEROVNET_OFFLOAD_ERR_UNSUPPORTED_L4_PROTOCOL = 5,
  AEROVNET_OFFLOAD_ERR_UNSUPPORTED_IPV6 = 6,
  AEROVNET_OFFLOAD_ERR_BAD_MSS = 7,
  AEROVNET_OFFLOAD_ERR_UNSUPPORTED_FRAGMENTATION = 8,
} AEROVNET_OFFLOAD_RESULT;

/*
 * Builds a virtio-net transmit header for the provided Ethernet frame.
 *
 * On success, writes a fully-populated virtio-net header to `out_hdr`. If no
 * offload is requested, `out_hdr` is written as all zeros.
 */
AEROVNET_OFFLOAD_RESULT AerovNetBuildTxVirtioNetHdr(const uint8_t* frame,
                                                    size_t frame_len,
                                                    const AEROVNET_TX_OFFLOAD_INTENT* intent,
                                                    AEROVNET_VIRTIO_NET_HDR* out_hdr,
                                                    AEROVNET_OFFLOAD_PARSE_INFO* out_info);
