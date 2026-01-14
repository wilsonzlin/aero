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

/*
 * Event types (virtio-snd specification).
 *
 * The Windows 7 Aero contract v1 does not currently define any event messages,
 * but the spec reserves eventq for asynchronous notifications. Define the
 * standard event types so the driver can parse/log future device models without
 * depending on them for correctness.
 */
#define VIRTIO_SND_EVT_JACK_CONNECTED     0x1000u
#define VIRTIO_SND_EVT_JACK_DISCONNECTED  0x1001u
#define VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED 0x1100u
#define VIRTIO_SND_EVT_PCM_XRUN           0x1101u
#define VIRTIO_SND_EVT_CTL_NOTIFY         0x1200u

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

/*
 * virtio-snd PCM format/rate enums.
 *
 * The Aero Windows 7 virtio-snd contract v1 requires S16/48kHz, but devices may
 * advertise additional formats/rates via PCM_INFO. The guest driver keeps the
 * full bitmasks and may negotiate other combinations when both the Windows
 * audio stack and the device support them.
 *
 * Values match the virtio-snd specification (`enum virtio_snd_pcm_fmt` /
 * `enum virtio_snd_pcm_rate`).
 */
#define VIRTIO_SND_PCM_FMT_IMA_ADPCM 0x00u
#define VIRTIO_SND_PCM_FMT_MU_LAW    0x01u
#define VIRTIO_SND_PCM_FMT_A_LAW     0x02u
#define VIRTIO_SND_PCM_FMT_S8        0x03u
#define VIRTIO_SND_PCM_FMT_U8        0x04u
#define VIRTIO_SND_PCM_FMT_S16       0x05u
#define VIRTIO_SND_PCM_FMT_U16       0x06u
#define VIRTIO_SND_PCM_FMT_S18_3     0x07u
#define VIRTIO_SND_PCM_FMT_U18_3     0x08u
#define VIRTIO_SND_PCM_FMT_S20_3     0x09u
#define VIRTIO_SND_PCM_FMT_U20_3     0x0Au
#define VIRTIO_SND_PCM_FMT_S24_3     0x0Bu
#define VIRTIO_SND_PCM_FMT_U24_3     0x0Cu
#define VIRTIO_SND_PCM_FMT_S20       0x0Du
#define VIRTIO_SND_PCM_FMT_U20       0x0Eu
#define VIRTIO_SND_PCM_FMT_S24       0x0Fu
#define VIRTIO_SND_PCM_FMT_U24       0x10u
#define VIRTIO_SND_PCM_FMT_S32       0x11u
#define VIRTIO_SND_PCM_FMT_U32       0x12u
#define VIRTIO_SND_PCM_FMT_FLOAT     0x13u
#define VIRTIO_SND_PCM_FMT_FLOAT64   0x14u
#define VIRTIO_SND_PCM_FMT_DSD_U8    0x15u
#define VIRTIO_SND_PCM_FMT_DSD_U16   0x16u
#define VIRTIO_SND_PCM_FMT_DSD_U32   0x17u

#define VIRTIO_SND_PCM_RATE_5512    0x00u
#define VIRTIO_SND_PCM_RATE_8000    0x01u
#define VIRTIO_SND_PCM_RATE_11025   0x02u
#define VIRTIO_SND_PCM_RATE_16000   0x03u
#define VIRTIO_SND_PCM_RATE_22050   0x04u
#define VIRTIO_SND_PCM_RATE_32000   0x05u
#define VIRTIO_SND_PCM_RATE_44100   0x06u
#define VIRTIO_SND_PCM_RATE_48000   0x07u
#define VIRTIO_SND_PCM_RATE_64000   0x08u
#define VIRTIO_SND_PCM_RATE_88200   0x09u
#define VIRTIO_SND_PCM_RATE_96000   0x0Au
#define VIRTIO_SND_PCM_RATE_176400  0x0Bu
#define VIRTIO_SND_PCM_RATE_192000  0x0Cu
#define VIRTIO_SND_PCM_RATE_384000  0x0Du
#define VIRTIO_SND_D_OUTPUT        0x00u
#define VIRTIO_SND_D_INPUT         0x01u
#define VIRTIO_SND_PLAYBACK_STREAM_ID 0u
#define VIRTIO_SND_CAPTURE_STREAM_ID  1u

/* PCM_INFO bitmask helpers (bits are indexed by the PCM_FMT/PCM_RATE values). */
#define VIRTIO_SND_PCM_FMT_MASK(_fmt)  (1ull << (_fmt))
#define VIRTIO_SND_PCM_RATE_MASK(_rate) (1ull << (_rate))

#define VIRTIO_SND_PCM_FMT_MASK_S16     VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_S16)
#define VIRTIO_SND_PCM_FMT_MASK_S24     VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_S24)
#define VIRTIO_SND_PCM_FMT_MASK_S32     VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_S32)
#define VIRTIO_SND_PCM_FMT_MASK_FLOAT   VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_FLOAT)
#define VIRTIO_SND_PCM_RATE_MASK_44100  VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_44100)
#define VIRTIO_SND_PCM_RATE_MASK_48000  VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_48000)
#define VIRTIO_SND_PCM_RATE_MASK_96000  VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_96000)

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

/* Event queue message header (virtio-snd spec). */
typedef struct _VIRTIO_SND_EVENT {
    ULONG type;
    ULONG data;
} VIRTIO_SND_EVENT, *PVIRTIO_SND_EVENT;
C_ASSERT(sizeof(VIRTIO_SND_EVENT) == 8);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_EVENT, type) == 0);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_EVENT, data) == 4);

#pragma pack(pop)

/*
 * Parsed event classification used by the driver. Unknown events are tolerated
 * and surfaced as VIRTIO_SND_EVENT_KIND_UNKNOWN.
 */
typedef enum _VIRTIO_SND_EVENT_KIND {
    VIRTIO_SND_EVENT_KIND_UNKNOWN = 0,
    VIRTIO_SND_EVENT_KIND_JACK_CONNECTED,
    VIRTIO_SND_EVENT_KIND_JACK_DISCONNECTED,
    VIRTIO_SND_EVENT_KIND_PCM_PERIOD_ELAPSED,
    VIRTIO_SND_EVENT_KIND_PCM_XRUN,
    VIRTIO_SND_EVENT_KIND_CTL_NOTIFY,
} VIRTIO_SND_EVENT_KIND;

typedef struct _VIRTIO_SND_EVENT_PARSED {
    ULONG Type;
    ULONG Data;
    VIRTIO_SND_EVENT_KIND Kind;
    /*
     * Event-specific interpretation of `Data` per virtio-snd specification.
     *
     * The union member is only valid for the corresponding Kind:
     *  - JACK_*: u.JackId
     *  - PCM_*:  u.StreamId
     *  - CTL_*:  u.CtlId
     */
    union {
        ULONG JackId;
        ULONG StreamId;
        ULONG CtlId;
    } u;
} VIRTIO_SND_EVENT_PARSED, *PVIRTIO_SND_EVENT_PARSED;

/* Parsed events are an internal representation, but keep their layout stable for host tests. */
C_ASSERT(sizeof(VIRTIO_SND_EVENT_PARSED) == 16);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_EVENT_PARSED, Type) == 0);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_EVENT_PARSED, Data) == 4);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_EVENT_PARSED, Kind) == 8);
C_ASSERT(FIELD_OFFSET(VIRTIO_SND_EVENT_PARSED, u) == 12);

#ifdef __cplusplus
extern "C" {
#endif

NTSTATUS VirtioSndStatusToNtStatus(_In_ ULONG virtio_status);
PCSTR VirtioSndStatusToString(_In_ ULONG virtio_status);

/*
 * Parse a single virtio-snd eventq message.
 *
 * The device may legally complete the buffer with extra trailing bytes. The
 * parser only requires BufferLen >= sizeof(VIRTIO_SND_EVENT) and ignores any
 * additional payload.
 */
_Must_inspect_result_ NTSTATUS VirtioSndParseEvent(
    _In_reads_bytes_(BufferLen) const void* Buffer,
    _In_ ULONG BufferLen,
    _Out_ VIRTIO_SND_EVENT_PARSED* OutEvent);

PCSTR VirtioSndEventTypeToString(_In_ ULONG virtio_event_type);

/*
 * Format mapping helpers.
 *
 * These helpers provide a minimal mapping between virtio-snd PCM format/rate
 * codes and their corresponding linear PCM properties so higher layers (WaveRT,
 * buffer sizing, etc) can reason about frame sizes.
 *
 * Note: The driver only uses a subset of formats; callers should treat FALSE as
 * "unknown/unsupported".
 */
_Must_inspect_result_ static __forceinline BOOLEAN VirtioSndPcmRateToHz(_In_ UCHAR Rate, _Out_ ULONG* RateHz)
{
    if (RateHz == NULL) {
        return FALSE;
    }

    switch (Rate) {
    case VIRTIO_SND_PCM_RATE_5512:
        *RateHz = 5512u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_8000:
        *RateHz = 8000u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_11025:
        *RateHz = 11025u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_16000:
        *RateHz = 16000u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_22050:
        *RateHz = 22050u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_32000:
        *RateHz = 32000u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_44100:
        *RateHz = 44100u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_48000:
        *RateHz = 48000u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_64000:
        *RateHz = 64000u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_88200:
        *RateHz = 88200u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_96000:
        *RateHz = 96000u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_176400:
        *RateHz = 176400u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_192000:
        *RateHz = 192000u;
        return TRUE;
    case VIRTIO_SND_PCM_RATE_384000:
        *RateHz = 384000u;
        return TRUE;
    default:
        *RateHz = 0;
        return FALSE;
    }
}

/*
 * Map a virtio-snd PCM format code to a byte size for a single sample.
 *
 * For the purposes of this driver, "sample" means one channel worth of audio
 * (so a frame is `Channels * BytesPerSample`).
 */
_Must_inspect_result_ static __forceinline BOOLEAN VirtioSndPcmFormatToBytesPerSample(_In_ UCHAR Format, _Out_ USHORT* BytesPerSample)
{
    if (BytesPerSample == NULL) {
        return FALSE;
    }

    switch (Format) {
    case VIRTIO_SND_PCM_FMT_MU_LAW:
    case VIRTIO_SND_PCM_FMT_A_LAW:
    case VIRTIO_SND_PCM_FMT_S8:
    case VIRTIO_SND_PCM_FMT_U8:
    case VIRTIO_SND_PCM_FMT_DSD_U8:
        *BytesPerSample = 1u;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_S16:
    case VIRTIO_SND_PCM_FMT_U16:
    case VIRTIO_SND_PCM_FMT_DSD_U16:
        *BytesPerSample = 2u;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_S24:
    case VIRTIO_SND_PCM_FMT_U24:
        /*
         * virtio-snd format codes are based on ALSA `snd_pcm_format_t`. In ALSA,
         * S24/U24 correspond to 24-bit samples stored in a 32-bit container
         * (`SNDRV_PCM_FORMAT_S24_LE` / `SNDRV_PCM_FORMAT_U24_LE`).
         */
        *BytesPerSample = 4u;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_S32:
    case VIRTIO_SND_PCM_FMT_U32:
    case VIRTIO_SND_PCM_FMT_FLOAT:
    case VIRTIO_SND_PCM_FMT_DSD_U32:
        *BytesPerSample = 4u;
        return TRUE;
    case VIRTIO_SND_PCM_FMT_FLOAT64:
        *BytesPerSample = 8u;
        return TRUE;
    default:
        *BytesPerSample = 0;
        return FALSE;
    }
}

/*
 * Map a virtio-snd PCM format code to the container bit width of a single sample.
 *
 * Note: virtio-snd format codes are based on ALSA `snd_pcm_format_t`. In ALSA,
 * S24/U24 correspond to 24-bit samples stored in a 32-bit container, so this
 * helper returns 32 for those formats (the valid bit width is 24).
 */
_Must_inspect_result_ static __forceinline BOOLEAN VirtioSndPcmFormatToBitsPerSample(_In_ UCHAR Format, _Out_ USHORT* BitsPerSample)
{
    USHORT bytes;
    if (BitsPerSample == NULL) {
        return FALSE;
    }
    if (!VirtioSndPcmFormatToBytesPerSample(Format, &bytes) || bytes == 0) {
        *BitsPerSample = 0;
        return FALSE;
    }
    *BitsPerSample = (USHORT)(bytes * 8u);
    return TRUE;
}

#ifdef __cplusplus
} // extern "C"
#endif
