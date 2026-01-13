#include "led_report_parse.h"

NTSTATUS virtio_input_parse_keyboard_led_output_report(unsigned char report_id, const unsigned char *buffer, size_t buffer_len,
                                                       unsigned char *led_bitfield_out)
{
    if (buffer == NULL || buffer_len == 0 || led_bitfield_out == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (buffer_len >= 2 && buffer[0] == report_id) {
        *led_bitfield_out = buffer[1];
        return STATUS_SUCCESS;
    }

    *led_bitfield_out = buffer[0];
    return STATUS_SUCCESS;
}
