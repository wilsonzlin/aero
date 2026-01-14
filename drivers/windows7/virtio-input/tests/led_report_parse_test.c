#include "../src/led_report_parse.h"

#include <assert.h>

static void test_report_id_prefixed_buffer(void)
{
    const unsigned char buf[] = {0x01, 0x02};
    unsigned char leds = 0;

    NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
    assert(status == STATUS_SUCCESS);
    assert(leds == 0x02);
}

static void test_single_byte_buffer(void)
{
    const unsigned char buf[] = {0x07};
    unsigned char leds = 0;

    NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
    assert(status == STATUS_SUCCESS);
    assert(leds == 0x07);
}

static void test_first_byte_not_report_id(void)
{
    const unsigned char buf[] = {0x02, 0x99};
    unsigned char leds = 0;

    NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
    assert(status == STATUS_SUCCESS);
    assert(leds == 0x02);
}

static void test_masks_padding_bits(void)
{
    /*
     * HID boot keyboard LED report defines 5 LED bits and 3 padding bits.
     * Some callers set the padding bits anyway; we must ignore them.
     */
    {
        const unsigned char buf[] = {0xFF};
        unsigned char leds = 0;

        NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
        assert(status == STATUS_SUCCESS);
        assert(leds == 0x1F);
    }
    {
        const unsigned char buf[] = {0x01, 0xFF};
        unsigned char leds = 0;

        NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
        assert(status == STATUS_SUCCESS);
        assert(leds == 0x1F);
    }
}

static void test_invalid_parameter(void)
{
    const unsigned char buf[] = {0x01, 0x02};
    unsigned char leds = 0;

    NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, NULL, sizeof(buf), &leds);
    assert(status == STATUS_INVALID_PARAMETER);

    status = virtio_input_parse_keyboard_led_output_report(0x01, buf, 0, &leds);
    assert(status == STATUS_INVALID_PARAMETER);

    status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), NULL);
    assert(status == STATUS_INVALID_PARAMETER);
}

int main(void)
{
    test_report_id_prefixed_buffer();
    test_single_byte_buffer();
    test_first_byte_not_report_id();
    test_masks_padding_bits();
    test_invalid_parameter();
    return 0;
}
