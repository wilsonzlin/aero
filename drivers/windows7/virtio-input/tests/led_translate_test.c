#include "../src/led_translate.h"

#include <assert.h>
#include <stdio.h>

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

static void assert_led_events_filtered(uint8_t bitfield, uint8_t supported_mask, size_t expect_led_count, const uint16_t *expect_codes,
                                        const uint32_t *expect_values) {
  struct virtio_input_event_le events[LED_TRANSLATE_EVENT_COUNT];

  size_t n = led_translate_build_virtio_events(bitfield, supported_mask, events);

  /* +1 for the mandatory EV_SYN flush. */
  assert(n == expect_led_count + 1);

  for (size_t i = 0; i < expect_led_count; ++i) {
    assert(events[i].type == to_le16((uint16_t)VIRTIO_INPUT_EV_LED));
    assert(events[i].code == to_le16(expect_codes[i]));
    assert(events[i].value == to_le32(expect_values[i]));
  }

  /* Final flush (always present). */
  assert(events[expect_led_count].type == to_le16((uint16_t)VIRTIO_INPUT_EV_SYN));
  assert(events[expect_led_count].code == to_le16((uint16_t)VIRTIO_INPUT_SYN_REPORT));
  assert(events[expect_led_count].value == to_le32(0));
}

static void assert_full_mask(uint8_t bitfield, uint32_t expect_numl, uint32_t expect_capsl, uint32_t expect_scrolll, uint32_t expect_compose,
                             uint32_t expect_kana) {
  const uint16_t codes[] = {VIRTIO_INPUT_LED_NUML, VIRTIO_INPUT_LED_CAPSL, VIRTIO_INPUT_LED_SCROLLL, VIRTIO_INPUT_LED_COMPOSE,
                            VIRTIO_INPUT_LED_KANA};
  const uint32_t values[] = {expect_numl, expect_capsl, expect_scrolll, expect_compose, expect_kana};
  assert_led_events_filtered(bitfield, 0x1Fu, 5, codes, values);
}

static void test_bit_mapping(void) {
  /*
   * Bit mapping: HID LED output bitfield -> virtio EV_LED codes.
   *
   * When the device advertises all 5 LED codes (0..4), we must emit 5 EV_LED
   * events (one per code) plus a final EV_SYN/SYN_REPORT (total: 6).
   */
  assert_full_mask(0x00u, 0, 0, 0, 0, 0);
  assert_full_mask(0x01u, 1, 0, 0, 0, 0);
  assert_full_mask(0x02u, 0, 1, 0, 0, 0);
  assert_full_mask(0x04u, 0, 0, 1, 0, 0);
  assert_full_mask(0x08u, 0, 0, 0, 1, 0);
  assert_full_mask(0x10u, 0, 0, 0, 0, 1);
  assert_full_mask(0x1Fu, 1, 1, 1, 1, 1);
  /* Padding bits in the HID output report byte should be ignored. */
  assert_full_mask(0xFFu, 1, 1, 1, 1, 1);

  /*
   * Filtering: only required LEDs advertised (Num/Caps/Scroll) => only emit
   * those 3 EV_LED events (+ EV_SYN).
   */
  {
    const uint16_t codes[] = {VIRTIO_INPUT_LED_NUML, VIRTIO_INPUT_LED_CAPSL, VIRTIO_INPUT_LED_SCROLLL};
    const uint32_t values[] = {1, 1, 1};
    assert_led_events_filtered(0x1Fu, 0x07u, 3, codes, values);
  }

  /*
   * Edge case: Compose/Kana bits set in the HID report, but not advertised by
   * the device => must not emit LED_COMPOSE/LED_KANA events.
   */
  {
    const uint16_t codes[] = {VIRTIO_INPUT_LED_NUML, VIRTIO_INPUT_LED_CAPSL, VIRTIO_INPUT_LED_SCROLLL};
    const uint32_t values[] = {0, 0, 0};
    assert_led_events_filtered(0x18u /* Compose|Kana */, 0x07u, 3, codes, values);
  }

  /*
   * Unknown supported mask (0) should fall back to emitting only required LEDs
   * (Num/Caps/Scroll) rather than all 5.
   */
  {
    const uint16_t codes[] = {VIRTIO_INPUT_LED_NUML, VIRTIO_INPUT_LED_CAPSL, VIRTIO_INPUT_LED_SCROLLL};
    const uint32_t values[] = {1, 1, 1};
    assert_led_events_filtered(0x1Fu, 0x00u, 3, codes, values);
  }
}

int main(void) {
  test_bit_mapping();
  printf("led_translate_test: ok\n");
  return 0;
}
