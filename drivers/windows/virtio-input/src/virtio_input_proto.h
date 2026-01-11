#pragma once

#include <ntddk.h>

typedef struct _VIRTIO_INPUT_EVENT {
    UINT16 Type;
    UINT16 Code;
    UINT32 Value;
} VIRTIO_INPUT_EVENT, *PVIRTIO_INPUT_EVENT;

/*
 * virtio-input device configuration layout (DEVICE_CFG capability).
 *
 * The device exposes a single config window with a selector scheme. The driver
 * writes Select/Subsel, then reads Size and Payload.
 */
typedef struct _VIRTIO_INPUT_CONFIG {
    UCHAR Select;
    UCHAR Subsel;
    UCHAR Size;
    UCHAR Reserved[5];
    UCHAR Payload[128];
} VIRTIO_INPUT_CONFIG, *PVIRTIO_INPUT_CONFIG;

C_ASSERT(sizeof(VIRTIO_INPUT_CONFIG) == 136);

/*
 * Required config selectors for Aero contract v1.
 *
 * Values match the upstream virtio-input specification.
 */
#define VIRTIO_INPUT_CFG_ID_NAME 0x01
#define VIRTIO_INPUT_CFG_ID_DEVIDS 0x03
#define VIRTIO_INPUT_CFG_EV_BITS 0x11

/*
 * ID_DEVIDS payload layout (little-endian fields on the wire; Win7 is LE).
 */
typedef struct _VIRTIO_INPUT_DEVIDS {
    USHORT Bustype;
    USHORT Vendor;
    USHORT Product;
    USHORT Version;
} VIRTIO_INPUT_DEVIDS, *PVIRTIO_INPUT_DEVIDS;

C_ASSERT(sizeof(VIRTIO_INPUT_DEVIDS) == 8);

#define VIRTIO_INPUT_DEVIDS_BUSTYPE_VIRTUAL 0x0006
#define VIRTIO_INPUT_DEVIDS_VENDOR_VIRTIO 0x1AF4
#define VIRTIO_INPUT_DEVIDS_PRODUCT_KEYBOARD 0x0001
#define VIRTIO_INPUT_DEVIDS_PRODUCT_MOUSE 0x0002
#define VIRTIO_INPUT_DEVIDS_VERSION 0x0001

// Linux input event types/codes are used by virtio-input.
#define VIRTIO_INPUT_EV_SYN 0x00
#define VIRTIO_INPUT_EV_KEY 0x01
#define VIRTIO_INPUT_EV_REL 0x02
#define VIRTIO_INPUT_EV_LED 0x11

#define VIRTIO_INPUT_SYN_REPORT 0

#define VIRTIO_INPUT_LED_NUML 0
#define VIRTIO_INPUT_LED_CAPSL 1
#define VIRTIO_INPUT_LED_SCROLLL 2
#define VIRTIO_INPUT_LED_COMPOSE 3
#define VIRTIO_INPUT_LED_KANA 4
