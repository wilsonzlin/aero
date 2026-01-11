/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

/*
 * Aero virtio-snd wire protocol subset used by the Windows 7 guest driver.
 *
 * Notes:
 * - All fields are little-endian on the wire.
 * - Structures are packed to match the device/emulator byte layout exactly.
 */

/* Queue indices (virtio-snd spec). */
#define VIRTIO_SND_QUEUE_CONTROL 0u
#define VIRTIO_SND_QUEUE_EVENT   1u
#define VIRTIO_SND_QUEUE_TX      2u
#define VIRTIO_SND_QUEUE_RX      3u

/* Control queue request codes (subset). */
#define VIRTIO_SND_R_PCM_INFO       0x0100u
#define VIRTIO_SND_R_PCM_SET_PARAMS 0x0101u
#define VIRTIO_SND_R_PCM_PREPARE    0x0102u
#define VIRTIO_SND_R_PCM_RELEASE    0x0103u
#define VIRTIO_SND_R_PCM_START      0x0104u
#define VIRTIO_SND_R_PCM_STOP       0x0105u

/* Control queue response status codes. */
#define VIRTIO_SND_S_OK       0u
#define VIRTIO_SND_S_BAD_MSG  1u
#define VIRTIO_SND_S_NOT_SUPP 2u
#define VIRTIO_SND_S_IO_ERR   3u

/* Fixed stream formats implemented by the emulator (Aero contract v1). */
#define VIRTIO_SND_PCM_FMT_S16     0x05u
#define VIRTIO_SND_PCM_RATE_48000  0x07u
#define VIRTIO_SND_D_OUTPUT        0x00u
#define VIRTIO_SND_D_INPUT         0x01u
#define VIRTIO_SND_PLAYBACK_STREAM_ID 0u
#define VIRTIO_SND_CAPTURE_STREAM_ID  1u

/* PCM_INFO bitmask helpers (bits are indexed by the PCM_FMT/PCM_RATE values). */
#define VIRTIO_SND_PCM_FMT_MASK_S16    (1ull << VIRTIO_SND_PCM_FMT_S16)
#define VIRTIO_SND_PCM_RATE_MASK_48000 (1ull << VIRTIO_SND_PCM_RATE_48000)

#pragma pack(push, 1)

/* VIRTIO_SND_R_PCM_INFO request. */
typedef struct _VIRTIO_SND_PCM_INFO_REQ {
    ULONG code;
    ULONG start_id;
    ULONG count;
} VIRTIO_SND_PCM_INFO_REQ, *PVIRTIO_SND_PCM_INFO_REQ;
C_ASSERT(sizeof(VIRTIO_SND_PCM_INFO_REQ) == 12);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO_REQ, code) == 0);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO_REQ, start_id) == 4);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO_REQ, count) == 8);

/* VIRTIO_SND_R_PCM_SET_PARAMS request. */
typedef struct _VIRTIO_SND_PCM_SET_PARAMS_REQ {
    ULONG code;
    ULONG stream_id;
    ULONG buffer_bytes;
    ULONG period_bytes;
    ULONG features;
    UCHAR channels;
    UCHAR format;
    UCHAR rate;
    UCHAR padding;
} VIRTIO_SND_PCM_SET_PARAMS_REQ, *PVIRTIO_SND_PCM_SET_PARAMS_REQ;
C_ASSERT(sizeof(VIRTIO_SND_PCM_SET_PARAMS_REQ) == 24);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, code) == 0);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, stream_id) == 4);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, buffer_bytes) == 8);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, period_bytes) == 12);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, features) == 16);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, channels) == 20);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, format) == 21);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, rate) == 22);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SET_PARAMS_REQ, padding) == 23);

/* VIRTIO_SND_R_PCM_{PREPARE,RELEASE,START,STOP} request. */
typedef struct _VIRTIO_SND_PCM_SIMPLE_REQ {
    ULONG code;
    ULONG stream_id;
} VIRTIO_SND_PCM_SIMPLE_REQ, *PVIRTIO_SND_PCM_SIMPLE_REQ;
C_ASSERT(sizeof(VIRTIO_SND_PCM_SIMPLE_REQ) == 8);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SIMPLE_REQ, code) == 0);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_SIMPLE_REQ, stream_id) == 4);

/* Generic control queue response header. */
typedef struct _VIRTIO_SND_HDR_RESP {
    ULONG status;
} VIRTIO_SND_HDR_RESP, *PVIRTIO_SND_HDR_RESP;
C_ASSERT(sizeof(VIRTIO_SND_HDR_RESP) == 4);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_HDR_RESP, status) == 0);

/* VIRTIO_SND_R_PCM_INFO response entry (matches emulator layout). */
typedef struct _VIRTIO_SND_PCM_INFO {
    ULONG stream_id;
    ULONG features;
    ULONGLONG formats;
    ULONGLONG rates;
    UCHAR direction;
    UCHAR channels_min;
    UCHAR channels_max;
    UCHAR reserved[5];
} VIRTIO_SND_PCM_INFO, *PVIRTIO_SND_PCM_INFO;

C_ASSERT(sizeof(VIRTIO_SND_PCM_INFO) == 32);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO, stream_id) == 0);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO, features) == 4);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO, formats) == 8);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO, rates) == 16);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO, direction) == 24);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO, channels_min) == 25);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO, channels_max) == 26);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_INFO, reserved) == 27);

/* TX/RX queue header preceding PCM data. */
typedef struct _VIRTIO_SND_TX_HDR {
    ULONG stream_id;
    ULONG reserved;
} VIRTIO_SND_TX_HDR, *PVIRTIO_SND_TX_HDR;
C_ASSERT(sizeof(VIRTIO_SND_TX_HDR) == 8);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_TX_HDR, stream_id) == 0);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_TX_HDR, reserved) == 4);

/* TX/RX queue status returned by device. */
typedef struct _VIRTIO_SND_PCM_STATUS {
    ULONG status;
    ULONG latency_bytes;
} VIRTIO_SND_PCM_STATUS, *PVIRTIO_SND_PCM_STATUS;
C_ASSERT(sizeof(VIRTIO_SND_PCM_STATUS) == 8);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_STATUS, status) == 0);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_PCM_STATUS, latency_bytes) == 4);

#pragma pack(pop)

#ifdef __cplusplus
extern "C" {
#endif

NTSTATUS VirtioSndStatusToNtStatus(_In_ ULONG virtio_status);
PCSTR VirtioSndStatusToString(_In_ ULONG virtio_status);

#ifdef __cplusplus
} // extern "C"
#endif
