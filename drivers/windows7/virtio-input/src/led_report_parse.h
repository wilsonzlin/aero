#pragma once

/*
 * Portable parsing of HID keyboard LED output reports.
 *
 * Windows HID write paths are inconsistent about whether the Report ID byte is
 * included in the report buffer:
 *   - Some callers pass [ReportID, LedBitfield]
 *   - Some callers pass [LedBitfield]
 *
 * The driver uses this helper to interpret either format without risking
 * out-of-bounds reads (the write IOCTL uses METHOD_NEITHER for user buffers).
 *
 * This header is intentionally self-contained so it can be compiled in host-side
 * unit tests without the Windows WDK.
 */

#include <stddef.h>

/*
 * On Windows driver builds, NTSTATUS and STATUS_* are provided by the WDK.
 * For host-side unit tests (Linux), provide a minimal compatible subset.
 */
#if defined(_WIN32)
#include <ntddk.h>
#else
#include <stdint.h>
#endif

#ifdef __cplusplus
extern "C" {
#endif

/*
 * NTSTATUS is provided by the WDK on Windows. For host-side unit tests, define
 * a minimal compatible subset.
 */
#if !defined(_WIN32)
typedef int32_t NTSTATUS;

#ifndef STATUS_SUCCESS
#define STATUS_SUCCESS ((NTSTATUS)0)
#endif

#ifndef STATUS_INVALID_PARAMETER
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#endif

#endif /* !_WIN32 */

/*
 * Parses a HID keyboard LED output report buffer.
 *
 * Inputs:
 *   - report_id: expected Report ID (currently 1 for the keyboard collection).
 *   - buffer/buffer_len: raw bytes as provided by the HID write API.
 *
 * Output:
 *   - led_bitfield_out: receives the LED bitfield (NumLock/CapsLock/etc).
 *
 * Note: The HID boot keyboard LED output report defines 5 LED bits (NumLock,
 * CapsLock, ScrollLock, Compose, Kana) and 3 padding bits. This helper masks
 * the parsed value to the 5 defined bits (0x1F).
 *
 * Behavior (legacy):
 *   - If buffer_len >= 2 and buffer[0] == report_id, treat buffer[1] as the LED
 *     bitfield.
 *   - Otherwise treat buffer[0] as the LED bitfield.
 *
 * Returns:
 *   - STATUS_SUCCESS on success.
 *   - STATUS_INVALID_PARAMETER if buffer is NULL, buffer_len == 0, or
 *     led_bitfield_out is NULL.
 */
NTSTATUS virtio_input_parse_keyboard_led_output_report(unsigned char report_id, const unsigned char *buffer, size_t buffer_len,
                                                       unsigned char *led_bitfield_out);

#ifdef __cplusplus
} /* extern "C" */
#endif
