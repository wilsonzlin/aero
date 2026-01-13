#include "../src/led_translate.h"

#include <assert.h>
#include <stdio.h>

static void assert_led_events(uint8_t bitfield, uint32_t expect_numl, uint32_t expect_capsl, uint32_t expect_scrolll, uint32_t expect_compose,
                              uint32_t expect_kana) {
  struct virtio_input_event_le events[LED_TRANSLATE_EVENT_COUNT];

  size_t n = led_translate_build_virtio_events(bitfield, events);
  assert(n == LED_TRANSLATE_EVENT_COUNT);

  /* 5x EV_LED, fixed order. */
  assert(events[0].type == (uint16_t)VIRTIO_INPUT_EV_LED);
  assert(events[0].code == (uint16_t)VIRTIO_INPUT_LED_NUML);
  assert(events[0].value == expect_numl);

  assert(events[1].type == (uint16_t)VIRTIO_INPUT_EV_LED);
  assert(events[1].code == (uint16_t)VIRTIO_INPUT_LED_CAPSL);
  assert(events[1].value == expect_capsl);

  assert(events[2].type == (uint16_t)VIRTIO_INPUT_EV_LED);
  assert(events[2].code == (uint16_t)VIRTIO_INPUT_LED_SCROLLL);
  assert(events[2].value == expect_scrolll);

  assert(events[3].type == (uint16_t)VIRTIO_INPUT_EV_LED);
  assert(events[3].code == (uint16_t)VIRTIO_INPUT_LED_COMPOSE);
  assert(events[3].value == expect_compose);

  assert(events[4].type == (uint16_t)VIRTIO_INPUT_EV_LED);
  assert(events[4].code == (uint16_t)VIRTIO_INPUT_LED_KANA);
  assert(events[4].value == expect_kana);

  /* Final flush. */
  assert(events[5].type == (uint16_t)VIRTIO_INPUT_EV_SYN);
  assert(events[5].code == (uint16_t)VIRTIO_INPUT_SYN_REPORT);
  assert(events[5].value == 0);
}

static void test_bit_mapping(void) {
  /* Single-bit cases. */
  assert_led_events(0x01u, 1, 0, 0, 0, 0);
  assert_led_events(0x02u, 0, 1, 0, 0, 0);
  assert_led_events(0x04u, 0, 0, 1, 0, 0);
  assert_led_events(0x08u, 0, 0, 0, 1, 0);
  assert_led_events(0x10u, 0, 0, 0, 0, 1);

  /* Multi-bit and empty cases. */
  assert_led_events(0x00u, 0, 0, 0, 0, 0);
  assert_led_events(0x1Fu, 1, 1, 1, 1, 1);
  assert_led_events(0xFFu, 1, 1, 1, 1, 1);
}

int main(void) {
  test_bit_mapping();
  printf("led_translate_test: ok\n");
  return 0;
}

