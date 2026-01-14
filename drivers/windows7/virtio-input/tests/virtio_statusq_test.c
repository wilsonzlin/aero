#include "../src/virtio_statusq.h"

#include <assert.h>
#include <stdio.h>

static void test_cookie_to_index_validation(void)
{
    uint8_t storage[128] = {0};
    uint8_t* base = storage + 16;
    size_t stride = 8;
    uint16_t count = 4;

    uint16_t idx = 0xFFFFu;

    /* Valid cookie. */
    assert(VirtioStatusQCookieToIndex(base, stride, count, base + (stride * 2), &idx));
    assert(idx == 2);

    /* Misaligned (points into a buffer, not at start). */
    idx = 0xFFFFu;
    assert(!VirtioStatusQCookieToIndex(base, stride, count, base + (stride * 2) + 1, &idx));
    assert(idx == 0xFFFFu);

    /* Out of range (below base). */
    idx = 0xFFFFu;
    assert(!VirtioStatusQCookieToIndex(base, stride, count, base - 1, &idx));

    /* Out of range (one past end). */
    idx = 0xFFFFu;
    assert(!VirtioStatusQCookieToIndex(base, stride, count, base + (stride * count), &idx));
}

static void test_coalescing_capacity_1_no_drop(void)
{
    VIOINPUT_STATUSQ_COALESCE_SIM sim;
    VirtioStatusQCoalesceSimInit(&sim, 1, false);

    /* First write submits immediately. */
    assert(VirtioStatusQCoalesceSimWrite(&sim, 0x01u));
    assert(sim.FreeCount == 0);
    assert(!sim.PendingValid);

    /* Two writes while full coalesce into the last pending value. */
    assert(!VirtioStatusQCoalesceSimWrite(&sim, 0x02u));
    assert(sim.PendingValid);
    assert(sim.PendingLedBitfield == 0x02u);

    assert(!VirtioStatusQCoalesceSimWrite(&sim, 0x04u));
    assert(sim.PendingValid);
    assert(sim.PendingLedBitfield == 0x04u);

    /* Completion triggers submission of the coalesced pending state. */
    assert(VirtioStatusQCoalesceSimComplete(&sim));
    assert(sim.FreeCount == 0);
    assert(!sim.PendingValid);

    /* Next completion frees the queue (no pending). */
    assert(!VirtioStatusQCoalesceSimComplete(&sim));
    assert(sim.FreeCount == 1);
}

static void test_coalescing_capacity_1_drop_on_full(void)
{
    VIOINPUT_STATUSQ_COALESCE_SIM sim;
    VirtioStatusQCoalesceSimInit(&sim, 1, true);

    assert(VirtioStatusQCoalesceSimWrite(&sim, 0x01u));
    assert(sim.FreeCount == 0);
    assert(!sim.PendingValid);

    /* Full queue -> drop pending immediately. */
    assert(!VirtioStatusQCoalesceSimWrite(&sim, 0x02u));
    assert(!sim.PendingValid);

    /* Completion does not trigger a submission because nothing is pending. */
    assert(!VirtioStatusQCoalesceSimComplete(&sim));
    assert(sim.FreeCount == 1);
}

static void test_coalescing_capacity_2_pending_submitted_on_completion(void)
{
    VIOINPUT_STATUSQ_COALESCE_SIM sim;
    VirtioStatusQCoalesceSimInit(&sim, 2, false);

    assert(VirtioStatusQCoalesceSimWrite(&sim, 0x01u));
    assert(sim.FreeCount == 1);
    assert(VirtioStatusQCoalesceSimWrite(&sim, 0x02u));
    assert(sim.FreeCount == 0);

    /* Queue is full now; next write becomes pending. */
    assert(!VirtioStatusQCoalesceSimWrite(&sim, 0x04u));
    assert(sim.PendingValid);

    /* One completion frees a slot and immediately submits the pending write. */
    assert(VirtioStatusQCoalesceSimComplete(&sim));
    assert(sim.FreeCount == 0);
    assert(!sim.PendingValid);
}

static void test_coalescing_capacity_2_drop_on_full(void)
{
    VIOINPUT_STATUSQ_COALESCE_SIM sim;
    VirtioStatusQCoalesceSimInit(&sim, 2, true);

    assert(VirtioStatusQCoalesceSimWrite(&sim, 0x01u));
    assert(sim.FreeCount == 1);
    assert(!sim.PendingValid);

    assert(VirtioStatusQCoalesceSimWrite(&sim, 0x02u));
    assert(sim.FreeCount == 0);
    assert(!sim.PendingValid);

    /* Full queue -> drop pending immediately. */
    assert(!VirtioStatusQCoalesceSimWrite(&sim, 0x04u));
    assert(sim.FreeCount == 0);
    assert(!sim.PendingValid);

    /* Completion frees a slot but does not submit anything because nothing is pending. */
    assert(!VirtioStatusQCoalesceSimComplete(&sim));
    assert(sim.FreeCount == 1);
    assert(!sim.PendingValid);

    /* Final completion returns us to the fully-free state. */
    assert(!VirtioStatusQCoalesceSimComplete(&sim));
    assert(sim.FreeCount == 2);
}

int main(void)
{
    test_cookie_to_index_validation();
    test_coalescing_capacity_1_no_drop();
    test_coalescing_capacity_1_drop_on_full();
    test_coalescing_capacity_2_pending_submitted_on_completion();
    test_coalescing_capacity_2_drop_on_full();
    printf("virtio_statusq_test: ok\n");
    return 0;
}
