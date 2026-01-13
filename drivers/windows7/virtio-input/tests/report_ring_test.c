// TEST_DEPS: virtio_input.c hid_translate.c

#include "../src/virtio_input.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

struct report_ready_counter {
  uint32_t calls;
};

static void report_ready_cb(void *context) {
  struct report_ready_counter *c = (struct report_ready_counter *)context;
  c->calls++;
}

struct lock_state {
  bool locked;
  uint32_t lock_calls;
  uint32_t unlock_calls;
};

static void lock_cb(void *context) {
  struct lock_state *s = (struct lock_state *)context;
  assert(!s->locked);
  s->locked = true;
  s->lock_calls++;
}

static void unlock_cb(void *context) {
  struct lock_state *s = (struct lock_state *)context;
  assert(s->locked);
  s->locked = false;
  s->unlock_calls++;
}

struct report_ready_and_lock {
  struct report_ready_counter ready;
  struct lock_state lock;
};

static void report_ready_assert_unlocked_cb(void *context) {
  struct report_ready_and_lock *c = (struct report_ready_and_lock *)context;
  assert(!c->lock.locked);
  c->ready.calls++;
}

#define REPORT_SEQ_BYTES 5u
_Static_assert((size_t)VIRTIO_INPUT_REPORT_MAX_SIZE >= (size_t)REPORT_SEQ_BYTES,
               "test expects enough space to encode a 32-bit sequence number");

static size_t report_len_for_seq(uint32_t seq) {
  /*
   * Use variable report lengths (but always include the sequence number) to
   * ensure the ring copies and preserves lengths correctly.
   */
  const size_t min_len = REPORT_SEQ_BYTES;
  const size_t span = (size_t)VIRTIO_INPUT_REPORT_MAX_SIZE - min_len + 1u;
  return min_len + (size_t)(seq % (uint32_t)span);
}

static void make_report(uint8_t out[VIRTIO_INPUT_REPORT_MAX_SIZE], uint32_t seq) {
  out[0] = 0xA5; /* constant marker */
  out[1] = (uint8_t)(seq & 0xFFu);
  out[2] = (uint8_t)((seq >> 8) & 0xFFu);
  out[3] = (uint8_t)((seq >> 16) & 0xFFu);
  out[4] = (uint8_t)((seq >> 24) & 0xFFu);

  for (size_t i = 5; i < (size_t)VIRTIO_INPUT_REPORT_MAX_SIZE; i++) {
    out[i] = (uint8_t)(seq + (uint32_t)i);
  }
}

static void expect_report_seq(const struct virtio_input_report *r, uint32_t seq) {
  uint8_t expected[VIRTIO_INPUT_REPORT_MAX_SIZE];
  const size_t expected_len = report_len_for_seq(seq);
  make_report(expected, seq);
  assert(r->len == (uint8_t)expected_len);
  assert(memcmp(r->data, expected, expected_len) == 0);
}

static void test_report_ring_drop_oldest(void) {
  struct virtio_input_device dev;
  struct report_ready_counter ready = {0};

  virtio_input_device_init(&dev, report_ready_cb, &ready, NULL, NULL, NULL);

  /*
   * Push more than capacity to force drops and wrap-around. The ring should
   * retain the newest VIRTIO_INPUT_REPORT_RING_CAPACITY reports and pop them
   * oldest-to-newest within that retained window.
   */
  const uint32_t total_reports = (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY * 3u + 7u;
  for (uint32_t seq = 0; seq < total_reports; seq++) {
    uint8_t report[VIRTIO_INPUT_REPORT_MAX_SIZE];
    const size_t report_len = report_len_for_seq(seq);
    make_report(report, seq);
    dev.translate.emit_report(dev.translate.emit_report_context, report, report_len);
  }

  assert(ready.calls == total_reports);
  assert(dev.report_ring.count == (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY);

  const uint32_t first_retained = total_reports - (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY;

  uint32_t popped = 0;
  struct virtio_input_report out;
  while (virtio_input_try_pop_report(&dev, &out)) {
    expect_report_seq(&out, first_retained + popped);
    popped++;
  }

  assert(popped == (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY);
  assert(dev.report_ring.count == 0);
}

static void test_report_ring_locking_and_oversize_drop(void) {
  struct virtio_input_device dev;
  struct report_ready_and_lock ctx;
  memset(&ctx, 0, sizeof(ctx));

  virtio_input_device_init(&dev, report_ready_assert_unlocked_cb, &ctx, lock_cb, unlock_cb, &ctx.lock);

  /* Oversize reports are rejected (should not acquire the lock or call callbacks). */
  {
    uint8_t oversize[VIRTIO_INPUT_REPORT_MAX_SIZE + 1u];
    memset(oversize, 0xCC, sizeof(oversize));
    dev.translate.emit_report(dev.translate.emit_report_context, oversize, sizeof(oversize));
    assert(ctx.ready.calls == 0);
    assert(ctx.lock.lock_calls == 0);
    assert(ctx.lock.unlock_calls == 0);
    assert(!ctx.lock.locked);
    assert(dev.report_ring.count == 0);
  }

  /* Normal reports should lock/unlock and call report_ready outside the lock. */
  for (uint32_t seq = 0; seq < 3; seq++) {
    uint8_t report[VIRTIO_INPUT_REPORT_MAX_SIZE];
    const size_t report_len = report_len_for_seq(seq);
    make_report(report, seq);
    dev.translate.emit_report(dev.translate.emit_report_context, report, report_len);
  }

  assert(ctx.ready.calls == 3);
  assert(ctx.lock.lock_calls == 3);
  assert(ctx.lock.unlock_calls == 3);
  assert(!ctx.lock.locked);
  assert(dev.report_ring.count == 3);

  /* Pop should also use the lock, even when empty. */
  for (uint32_t seq = 0; seq < 3; seq++) {
    struct virtio_input_report out;
    assert(virtio_input_try_pop_report(&dev, &out));
    expect_report_seq(&out, seq);
  }
  assert(dev.report_ring.count == 0);
  assert(ctx.ready.calls == 3);
  assert(ctx.lock.lock_calls == 6);
  assert(ctx.lock.unlock_calls == 6);

  {
    struct virtio_input_report out;
    assert(!virtio_input_try_pop_report(&dev, &out));
  }
  assert(ctx.lock.lock_calls == 7);
  assert(ctx.lock.unlock_calls == 7);
}

static void test_report_ring_drop_oldest_with_lock(void) {
  struct virtio_input_device dev;
  struct report_ready_and_lock ctx;
  memset(&ctx, 0, sizeof(ctx));

  virtio_input_device_init(&dev, report_ready_assert_unlocked_cb, &ctx, lock_cb, unlock_cb, &ctx.lock);

  const uint32_t total_reports = (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY + 9u;
  for (uint32_t seq = 0; seq < total_reports; seq++) {
    uint8_t report[VIRTIO_INPUT_REPORT_MAX_SIZE];
    const size_t report_len = report_len_for_seq(seq);
    make_report(report, seq);
    dev.translate.emit_report(dev.translate.emit_report_context, report, report_len);
    assert(dev.report_ring.count <= (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY);
  }

  assert(ctx.ready.calls == total_reports);
  assert(ctx.lock.lock_calls == total_reports);
  assert(ctx.lock.unlock_calls == total_reports);
  assert(!ctx.lock.locked);

  assert(dev.report_ring.count == (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY);
  assert(dev.report_ring.head == dev.report_ring.tail);

  const uint32_t first_retained = total_reports - (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY;
  for (uint32_t i = 0; i < (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY; i++) {
    struct virtio_input_report out;
    assert(virtio_input_try_pop_report(&dev, &out));
    expect_report_seq(&out, first_retained + i);
  }
  assert(dev.report_ring.count == 0);
  assert(dev.report_ring.head == dev.report_ring.tail);

  /* Pops also acquire/release the lock. */
  assert(ctx.lock.lock_calls == total_reports + (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY);
  assert(ctx.lock.unlock_calls == total_reports + (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY);
  assert(ctx.ready.calls == total_reports);

  {
    struct virtio_input_report out;
    assert(!virtio_input_try_pop_report(&dev, &out));
  }
  assert(ctx.lock.lock_calls == total_reports + (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY + 1u);
  assert(ctx.lock.unlock_calls == total_reports + (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY + 1u);
}

static void test_report_ring_drop_oldest_after_pop(void) {
  struct virtio_input_device dev;
  struct report_ready_counter ready = {0};
  virtio_input_device_init(&dev, report_ready_cb, &ready, NULL, NULL, NULL);

  const uint32_t cap = (uint32_t)VIRTIO_INPUT_REPORT_RING_CAPACITY;

  /* Fill the ring. */
  for (uint32_t seq = 0; seq < cap; seq++) {
    uint8_t report[VIRTIO_INPUT_REPORT_MAX_SIZE];
    const size_t report_len = report_len_for_seq(seq);
    make_report(report, seq);
    dev.translate.emit_report(dev.translate.emit_report_context, report, report_len);
  }
  assert(dev.report_ring.count == cap);
  assert(dev.report_ring.head == dev.report_ring.tail);

  /* Pop a few, then overflow again. */
  const uint32_t popped_first = 10;
  for (uint32_t seq = 0; seq < popped_first; seq++) {
    struct virtio_input_report out;
    assert(virtio_input_try_pop_report(&dev, &out));
    expect_report_seq(&out, seq);
  }
  assert(dev.report_ring.count == cap - popped_first);

  const uint32_t pushed_next = 20;
  for (uint32_t seq = cap; seq < cap + pushed_next; seq++) {
    uint8_t report[VIRTIO_INPUT_REPORT_MAX_SIZE];
    const size_t report_len = report_len_for_seq(seq);
    make_report(report, seq);
    dev.translate.emit_report(dev.translate.emit_report_context, report, report_len);
    assert(dev.report_ring.count <= cap);
  }
  assert(dev.report_ring.count == cap);
  assert(dev.report_ring.head == dev.report_ring.tail);

  /*
   * Initial retained window after the first pops is [popped_first, cap-1].
   * Pushing pushed_next causes pushed_next - popped_first drops once the ring is full.
   */
  const uint32_t first_retained = popped_first + (pushed_next - popped_first);
  for (uint32_t seq = first_retained; seq < cap + pushed_next; seq++) {
    struct virtio_input_report out;
    assert(virtio_input_try_pop_report(&dev, &out));
    expect_report_seq(&out, seq);
  }
  assert(dev.report_ring.count == 0);
  assert(dev.report_ring.head == dev.report_ring.tail);

  assert(ready.calls == cap + pushed_next);
}

int main(void) {
  test_report_ring_drop_oldest();
  test_report_ring_locking_and_oversize_drop();
  test_report_ring_drop_oldest_with_lock();
  test_report_ring_drop_oldest_after_pop();
  printf("report_ring_test: ok\n");
  return 0;
}
