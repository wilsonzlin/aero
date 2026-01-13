#include "led_translate.h"

size_t led_translate_build_virtio_events(uint8_t hid_led_bitfield, struct virtio_input_event_le *events) {
  size_t event_count = 0;

  events[event_count].type = (uint16_t)VIRTIO_INPUT_EV_LED;
  events[event_count].code = (uint16_t)VIRTIO_INPUT_LED_NUML;
  events[event_count].value = (uint32_t)((hid_led_bitfield & 0x01u) ? 1u : 0u);
  event_count++;

  events[event_count].type = (uint16_t)VIRTIO_INPUT_EV_LED;
  events[event_count].code = (uint16_t)VIRTIO_INPUT_LED_CAPSL;
  events[event_count].value = (uint32_t)((hid_led_bitfield & 0x02u) ? 1u : 0u);
  event_count++;

  events[event_count].type = (uint16_t)VIRTIO_INPUT_EV_LED;
  events[event_count].code = (uint16_t)VIRTIO_INPUT_LED_SCROLLL;
  events[event_count].value = (uint32_t)((hid_led_bitfield & 0x04u) ? 1u : 0u);
  event_count++;

  events[event_count].type = (uint16_t)VIRTIO_INPUT_EV_LED;
  events[event_count].code = (uint16_t)VIRTIO_INPUT_LED_COMPOSE;
  events[event_count].value = (uint32_t)((hid_led_bitfield & 0x08u) ? 1u : 0u);
  event_count++;

  events[event_count].type = (uint16_t)VIRTIO_INPUT_EV_LED;
  events[event_count].code = (uint16_t)VIRTIO_INPUT_LED_KANA;
  events[event_count].value = (uint32_t)((hid_led_bitfield & 0x10u) ? 1u : 0u);
  event_count++;

  events[event_count].type = (uint16_t)VIRTIO_INPUT_EV_SYN;
  events[event_count].code = (uint16_t)VIRTIO_INPUT_SYN_REPORT;
  events[event_count].value = 0;
  event_count++;

  return event_count;
}

