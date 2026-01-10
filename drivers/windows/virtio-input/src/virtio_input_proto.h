#pragma once

#include <ntddk.h>

typedef struct _VIRTIO_INPUT_EVENT {
    UINT16 Type;
    UINT16 Code;
    UINT32 Value;
} VIRTIO_INPUT_EVENT, *PVIRTIO_INPUT_EVENT;

// Linux input event types/codes are used by virtio-input.
#define VIRTIO_INPUT_EV_SYN 0x00
#define VIRTIO_INPUT_EV_LED 0x11

#define VIRTIO_INPUT_SYN_REPORT 0

#define VIRTIO_INPUT_LED_NUML 0
#define VIRTIO_INPUT_LED_CAPSL 1
#define VIRTIO_INPUT_LED_SCROLLL 2
#define VIRTIO_INPUT_LED_COMPOSE 3
#define VIRTIO_INPUT_LED_KANA 4

