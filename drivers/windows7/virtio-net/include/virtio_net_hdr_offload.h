#pragma once

/*
 * Portable virtio-net header/offload helpers.
 *
 * This module intentionally avoids any WDK/NDIS dependencies so that it can be
 * unit tested on the host (Linux/macOS) and reused by driver code.
 */

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Minimal virtio-net header (`struct virtio_net_hdr`) */
#pragma pack(push, 1)
typedef struct _VIRTIO_NET_HDR {
  uint8_t Flags;
  uint8_t GsoType;
  uint16_t HdrLen;
  uint16_t GsoSize;
  uint16_t CsumStart;
  uint16_t CsumOffset;
} VIRTIO_NET_HDR;
#pragma pack(pop)

/* Portable static assert for C99/MSVC. */
#define VIRTIO_NET_HDR_OFFLOAD_STATIC_ASSERT(name, cond) typedef char name[(cond) ? 1 : -1]
VIRTIO_NET_HDR_OFFLOAD_STATIC_ASSERT(virtio_net_hdr_must_be_10_bytes, sizeof(VIRTIO_NET_HDR) == 10);

/* virtio-net header Flags */
#define VIRTIO_NET_HDR_F_NEEDS_CSUM 0x01u
#define VIRTIO_NET_HDR_F_DATA_VALID 0x02u

/* virtio-net header GsoType */
#define VIRTIO_NET_HDR_GSO_NONE 0x00u
#define VIRTIO_NET_HDR_GSO_TCPV4 0x01u
#define VIRTIO_NET_HDR_GSO_UDP 0x03u
#define VIRTIO_NET_HDR_GSO_TCPV6 0x04u
#define VIRTIO_NET_HDR_GSO_ECN 0x80u

typedef enum _VIRTIO_NET_HDR_OFFLOAD_STATUS {
  VIRTIO_NET_HDR_OFFLOAD_STATUS_OK = 0,
  VIRTIO_NET_HDR_OFFLOAD_STATUS_INVALID_ARGUMENT = -1,
  VIRTIO_NET_HDR_OFFLOAD_STATUS_TRUNCATED = -2,
  VIRTIO_NET_HDR_OFFLOAD_STATUS_MALFORMED = -3,
  VIRTIO_NET_HDR_OFFLOAD_STATUS_UNSUPPORTED = -4,
} VIRTIO_NET_HDR_OFFLOAD_STATUS;

typedef enum _VIRTIO_NET_HDR_OFFLOAD_L3 {
  VIRTIO_NET_HDR_OFFLOAD_L3_UNKNOWN = 0,
  VIRTIO_NET_HDR_OFFLOAD_L3_IPV4 = 4,
  VIRTIO_NET_HDR_OFFLOAD_L3_IPV6 = 6,
} VIRTIO_NET_HDR_OFFLOAD_L3;

typedef struct _VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO {
  /* L2 */
  uint16_t L2Len;

  /* L3 */
  uint16_t L3Offset;
  uint16_t L3Len; /* IPv4 header length or IPv6 header+extensions length */
  uint8_t L3Proto; /* VIRTIO_NET_HDR_OFFLOAD_L3_* */

  /* L4 */
  uint16_t L4Offset;
  uint16_t L4Len;
  uint8_t L4Proto; /* IP protocol number (e.g. TCP=6, UDP=17) */

  /* Payload */
  uint16_t PayloadOffset;

  /* L4 checksum location (relative to start of Ethernet frame) */
  uint16_t CsumStart;
  uint16_t CsumOffset; /* relative to CsumStart */
} VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO;

typedef struct _VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST {
  /* Request that the device compute the L4 checksum (TCP/UDP). */
  uint8_t NeedsCsum;

  /* Request TSO (TCP segmentation offload). Only TCP is supported. */
  uint8_t Tso;
  uint8_t TsoEcn;
  uint16_t TsoMss;
} VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST;

typedef struct _VIRTIO_NET_HDR_OFFLOAD_RX_INFO {
  uint8_t NeedsCsum;
  uint8_t CsumValid;

  uint8_t IsGso;
  uint8_t GsoType; /* base type with ECN bit stripped */
  uint8_t GsoEcn;
  uint16_t GsoSize;
  uint16_t HdrLen;
} VIRTIO_NET_HDR_OFFLOAD_RX_INFO;

/*
 * Parse an Ethernet frame (with up to 2 VLAN tags) and locate the L3/L4 headers.
 * Offsets are relative to the beginning of the Ethernet frame.
 */
VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseFrame(const uint8_t* Frame, size_t FrameLen, VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info);

/*
 * Compute virtio-net header fields for checksum offload and/or TSO.
 *
 * If TxReq->NeedsCsum is set, the header is configured for L4 checksum offload.
 * If TxReq->Tso is set, the header is configured for TCP segmentation (TSO) and
 * checksum offload (checksum is required for TSO).
 */
VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadBuildTxHdr(const VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO* Info,
                                                            const VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST* TxReq,
                                                            VIRTIO_NET_HDR* Hdr);

/* Convenience: Parse + Build. */
VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadBuildTxHdrFromFrame(const uint8_t* Frame, size_t FrameLen,
                                                                     const VIRTIO_NET_HDR_OFFLOAD_TX_REQUEST* TxReq, VIRTIO_NET_HDR* Hdr);

/* Parse a received virtio-net header into a high-level offload summary. */
VIRTIO_NET_HDR_OFFLOAD_STATUS VirtioNetHdrOffloadParseRxHdr(const VIRTIO_NET_HDR* Hdr, VIRTIO_NET_HDR_OFFLOAD_RX_INFO* Info);

/* Explicitly zero a header (useful for non-offload packets). */
void VirtioNetHdrOffloadZero(VIRTIO_NET_HDR* Hdr);

#ifdef __cplusplus
} /* extern "C" */
#endif
