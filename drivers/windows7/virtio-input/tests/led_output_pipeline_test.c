// TEST_DEPS: led_report_parse.c led_translate.c

#include <assert.h>
#include <stdint.h>
#include <stdio.h>

#include "../src/led_report_parse.h"
#include "../src/led_translate.h"

static uint16_t to_le16(uint16_t v) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
  return (uint16_t)((v >> 8) | (v << 8));
#else
  return v;
#endif
}

static uint32_t to_le32(uint32_t v) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
  return ((v & 0x000000FFu) << 24) | ((v & 0x0000FF00u) << 8) | ((v & 0x00FF0000u) >> 8) | ((v & 0xFF000000u) >> 24);
#else
  return v;
#endif
}

static void assert_events(uint8_t bitfield, uint32_t expect_numl, uint32_t expect_capsl, uint32_t expect_scrolll, uint32_t expect_compose,
                          uint32_t expect_kana) {
  struct virtio_input_event_le events[LED_TRANSLATE_EVENT_COUNT];
  /* Device advertises all 5 LED codes (0..4). */
  size_t n = led_translate_build_virtio_events(bitfield, 0x1Fu, events);
  assert(n == LED_TRANSLATE_EVENT_COUNT);

  assert(events[0].type == to_le16((uint16_t)VIRTIO_INPUT_EV_LED));
  assert(events[0].code == to_le16((uint16_t)VIRTIO_INPUT_LED_NUML));
  assert(events[0].value == to_le32(expect_numl));

  assert(events[1].type == to_le16((uint16_t)VIRTIO_INPUT_EV_LED));
  assert(events[1].code == to_le16((uint16_t)VIRTIO_INPUT_LED_CAPSL));
  assert(events[1].value == to_le32(expect_capsl));

  assert(events[2].type == to_le16((uint16_t)VIRTIO_INPUT_EV_LED));
  assert(events[2].code == to_le16((uint16_t)VIRTIO_INPUT_LED_SCROLLL));
  assert(events[2].value == to_le32(expect_scrolll));

  assert(events[3].type == to_le16((uint16_t)VIRTIO_INPUT_EV_LED));
  assert(events[3].code == to_le16((uint16_t)VIRTIO_INPUT_LED_COMPOSE));
  assert(events[3].value == to_le32(expect_compose));

  assert(events[4].type == to_le16((uint16_t)VIRTIO_INPUT_EV_LED));
  assert(events[4].code == to_le16((uint16_t)VIRTIO_INPUT_LED_KANA));
  assert(events[4].value == to_le32(expect_kana));

  assert(events[5].type == to_le16((uint16_t)VIRTIO_INPUT_EV_SYN));
  assert(events[5].code == to_le16((uint16_t)VIRTIO_INPUT_SYN_REPORT));
  assert(events[5].value == to_le32(0));
}

static void test_prefixed_report_id_buffer(void) {
  /* Report buffer includes the ReportID byte. */
  const unsigned char buf[] = {0x01, 0x1F};
  unsigned char leds = 0;

  NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
  assert(status == STATUS_SUCCESS);
  assert(leds == 0x1F);

  assert_events((uint8_t)leds, 1, 1, 1, 1, 1);
}

static void test_single_byte_buffer(void) {
  /* Report buffer omits the ReportID byte. */
  const unsigned char buf[] = {0x1F};
  unsigned char leds = 0;

  NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
  assert(status == STATUS_SUCCESS);
  assert(leds == 0x1F);

  assert_events((uint8_t)leds, 1, 1, 1, 1, 1);
}

static void test_padding_bits_are_masked(void) {
  /*
   * HID boot keyboard LED output report defines 5 LED bits and 3 padding bits.
   * Some callers set the padding bits anyway; we must ignore them.
   */
  const unsigned char buf[] = {0x01, 0xFF};
  unsigned char leds = 0;

  NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
  assert(status == STATUS_SUCCESS);
  assert(leds == 0x1F);

  assert_events((uint8_t)leds, 1, 1, 1, 1, 1);
}

static void test_first_byte_not_report_id(void) {
  /*
   * When buffer[0] doesn't match report_id, parsing treats buffer[0] as the LED
   * bitfield (legacy HID write behavior).
   */
  const unsigned char buf[] = {0x02, 0x1F};
  unsigned char leds = 0;

  NTSTATUS status = virtio_input_parse_keyboard_led_output_report(0x01, buf, sizeof(buf), &leds);
  assert(status == STATUS_SUCCESS);
  assert(leds == 0x02);

  assert_events((uint8_t)leds, 0, 1, 0, 0, 0);
}

int main(void) {
  test_prefixed_report_id_buffer();
  test_single_byte_buffer();
  test_padding_bits_are_masked();
  test_first_byte_not_report_id();
  printf("led_output_pipeline_test: ok\n");
  return 0;
}
