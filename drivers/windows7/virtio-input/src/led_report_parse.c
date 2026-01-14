#include "led_report_parse.h"

NTSTATUS virtio_input_parse_keyboard_led_output_report(unsigned char report_id, const unsigned char *buffer, size_t buffer_len,
                                                       unsigned char *led_bitfield_out)
{
    const unsigned char LED_MASK = 0x1Fu; /* 5 defined HID boot keyboard LED bits (Num/Caps/Scroll/Compose/Kana). */

    if (buffer == NULL || buffer_len == 0 || led_bitfield_out == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (buffer_len >= 2 && buffer[0] == report_id) {
        *led_bitfield_out = (unsigned char)(buffer[1] & LED_MASK);
        return STATUS_SUCCESS;
    }

    *led_bitfield_out = (unsigned char)(buffer[0] & LED_MASK);
    return STATUS_SUCCESS;
}
