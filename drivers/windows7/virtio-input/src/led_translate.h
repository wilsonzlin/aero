#pragma once

/*
 * HID keyboard LED output report -> virtio-input EV_LED translation.
 *
 * The Win7 virtio-input driver receives HID output reports (NumLock, CapsLock,
 * etc) from the OS and forwards them to the guest via the virtio "statusq"
 * (virtqueue).
 *
 * This module is intentionally written as portable C so it can be unit-tested
 * on the host (gcc/clang) while also being compiled into the Win7 KMDF driver
 * (WDK/MSVC).
 */

#include "hid_translate.h"

#include <stddef.h>

#if defined(_WIN32)
/*
 * In the Win7 KMDF driver build, pull the virtio-input ABI constants from the
 * existing kernel header to avoid drift and macro redefinition warnings.
 *
 * (This header is not portable to host builds due to WDK dependencies, so
 * include it only on Windows.)
 */
#include "virtio_input_proto.h"
#endif

#ifdef __cplusplus
extern "C" {
#endif

/*
 * virtio-input event constants for LED output (subset).
 *
 * These match the upstream virtio-input specification (Linux input ABI).
 *
 * Note: The driver also has a separate kernel-only header defining these values
 * (virtio_input_proto.h). Keep these definitions guarded to avoid macro
 * redefinition warnings when building the driver.
 */

#if !defined(_WIN32)
#ifndef VIRTIO_INPUT_EV_LED
#define VIRTIO_INPUT_EV_LED 0x11
#endif

#ifndef VIRTIO_INPUT_LED_NUML
#define VIRTIO_INPUT_LED_NUML 0
#endif
#ifndef VIRTIO_INPUT_LED_CAPSL
#define VIRTIO_INPUT_LED_CAPSL 1
#endif
#ifndef VIRTIO_INPUT_LED_SCROLLL
#define VIRTIO_INPUT_LED_SCROLLL 2
#endif
#ifndef VIRTIO_INPUT_LED_COMPOSE
#define VIRTIO_INPUT_LED_COMPOSE 3
#endif
#ifndef VIRTIO_INPUT_LED_KANA
#define VIRTIO_INPUT_LED_KANA 4
#endif
#endif /* !_WIN32 */

enum { LED_TRANSLATE_EVENT_COUNT = 6 };

/*
 * Builds a virtio-input event sequence for a USB HID keyboard LED output report.
 *
 * Input is the HID LED bitfield byte:
 *   bit0: NumLock
 *   bit1: CapsLock
 *   bit2: ScrollLock
 *   bit3: Compose
 *   bit4: Kana
 *
 * The driver should only emit EV_LED events for LED codes advertised by the
 * virtio-input device via EV_BITS(EV_LED). `led_supported_mask` is a 5-bit mask
 * for codes 0..4 (bit N => LED code N supported).
 *
 * Output is:
 *   - 0..5x EV_LED events (in ascending LED code order) with value 0/1
 *   - 1x EV_SYN/SYN_REPORT event
 *
 * If `led_supported_mask` is 0 (unknown), this function falls back to emitting
 * only the required LED codes (NumLock/CapsLock/ScrollLock). This is safer than
 * emitting optional LEDs the device did not advertise.
 *
 * Caller must provide an output array of at least LED_TRANSLATE_EVENT_COUNT.
 *
 * Note: The output struct type is `virtio_input_event_le`. This function writes
 * the fields in little-endian encoding (CPU->LE) so the resulting buffer can be
 * sent directly over the virtio statusq as-is.
 */
size_t led_translate_build_virtio_events(uint8_t hid_led_bitfield, uint8_t led_supported_mask, struct virtio_input_event_le *events);

#ifdef __cplusplus
} /* extern "C" */
#endif
