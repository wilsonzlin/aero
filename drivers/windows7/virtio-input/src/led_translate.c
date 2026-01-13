#include "led_translate.h"

static uint16_t led_translate_cpu_to_le16(uint16_t v) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
  return (uint16_t)((v >> 8) | (v << 8));
#else
  return v;
#endif
}

static uint32_t led_translate_cpu_to_le32(uint32_t v) {
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
  return ((v & 0x000000FFu) << 24) | ((v & 0x0000FF00u) << 8) | ((v & 0x00FF0000u) >> 8) | ((v & 0xFF000000u) >> 24);
#else
  return v;
#endif
}

size_t led_translate_build_virtio_events(uint8_t hid_led_bitfield, struct virtio_input_event_le *events) {
  size_t event_count = 0;

  events[event_count].type = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_EV_LED);
  events[event_count].code = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_LED_NUML);
  events[event_count].value = led_translate_cpu_to_le32((uint32_t)((hid_led_bitfield & 0x01u) ? 1u : 0u));
  event_count++;

  events[event_count].type = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_EV_LED);
  events[event_count].code = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_LED_CAPSL);
  events[event_count].value = led_translate_cpu_to_le32((uint32_t)((hid_led_bitfield & 0x02u) ? 1u : 0u));
  event_count++;

  events[event_count].type = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_EV_LED);
  events[event_count].code = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_LED_SCROLLL);
  events[event_count].value = led_translate_cpu_to_le32((uint32_t)((hid_led_bitfield & 0x04u) ? 1u : 0u));
  event_count++;

  events[event_count].type = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_EV_LED);
  events[event_count].code = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_LED_COMPOSE);
  events[event_count].value = led_translate_cpu_to_le32((uint32_t)((hid_led_bitfield & 0x08u) ? 1u : 0u));
  event_count++;

  events[event_count].type = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_EV_LED);
  events[event_count].code = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_LED_KANA);
  events[event_count].value = led_translate_cpu_to_le32((uint32_t)((hid_led_bitfield & 0x10u) ? 1u : 0u));
  event_count++;

  events[event_count].type = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_EV_SYN);
  events[event_count].code = led_translate_cpu_to_le16((uint16_t)VIRTIO_INPUT_SYN_REPORT);
  events[event_count].value = led_translate_cpu_to_le32(0);
  event_count++;

  return event_count;
}
