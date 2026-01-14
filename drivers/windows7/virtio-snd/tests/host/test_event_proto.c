/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtio_snd_proto.h"

static void test_event_struct_packing_and_endianness(void)
{
    VIRTIO_SND_EVENT evt;

    RtlZeroMemory(&evt, sizeof(evt));
    evt.type = VIRTIO_SND_EVT_JACK_CONNECTED;
    evt.data = 0x11223344u;

    TEST_ASSERT(sizeof(evt) == 8);

    /*
     * virtio-snd events are always little-endian on the wire.
     * Windows 7 guests are little-endian, so the in-memory layout matches.
     */
    {
        const uint8_t expected[] = {
            0x00, 0x10, 0x00, 0x00, /* type = 0x1000 */
            0x44, 0x33, 0x22, 0x11, /* data = 0x11223344 */
        };
        TEST_ASSERT_MEMEQ(&evt, expected, sizeof(expected));
    }
}

static void test_parse_known_event_types(void)
{
    VIRTIO_SND_EVENT_PARSED out;
    NTSTATUS status;

    /* JACK_CONNECTED */
    {
        const uint8_t buf[] = {
            0x00, 0x10, 0x00, 0x00, /* type */
            0x05, 0x00, 0x00, 0x00, /* data = jack_id (5) */
        };
        status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), &out);
        TEST_ASSERT(status == STATUS_SUCCESS);
        TEST_ASSERT(out.Kind == VIRTIO_SND_EVENT_KIND_JACK_CONNECTED);
        TEST_ASSERT_EQ_U32(out.Type, VIRTIO_SND_EVT_JACK_CONNECTED);
        TEST_ASSERT_EQ_U32(out.Data, 5u);
        TEST_ASSERT_EQ_U32(out.u.JackId, 5u);
    }

    /* JACK_DISCONNECTED */
    {
        const uint8_t buf[] = {
            0x01, 0x10, 0x00, 0x00, /* type */
            0x07, 0x00, 0x00, 0x00, /* data = jack_id (7) */
        };
        status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), &out);
        TEST_ASSERT(status == STATUS_SUCCESS);
        TEST_ASSERT(out.Kind == VIRTIO_SND_EVENT_KIND_JACK_DISCONNECTED);
        TEST_ASSERT_EQ_U32(out.Type, VIRTIO_SND_EVT_JACK_DISCONNECTED);
        TEST_ASSERT_EQ_U32(out.Data, 7u);
        TEST_ASSERT_EQ_U32(out.u.JackId, 7u);
    }

    /* PCM_PERIOD_ELAPSED */
    {
        const uint8_t buf[] = {
            0x00, 0x11, 0x00, 0x00, /* type */
            0x00, 0x00, 0x00, 0x00, /* data = stream_id (0) */
        };
        status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), &out);
        TEST_ASSERT(status == STATUS_SUCCESS);
        TEST_ASSERT(out.Kind == VIRTIO_SND_EVENT_KIND_PCM_PERIOD_ELAPSED);
        TEST_ASSERT_EQ_U32(out.Type, VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED);
        TEST_ASSERT_EQ_U32(out.Data, 0u);
        TEST_ASSERT_EQ_U32(out.u.StreamId, 0u);
    }

    /* PCM_XRUN */
    {
        const uint8_t buf[] = {
            0x01, 0x11, 0x00, 0x00, /* type */
            0x01, 0x00, 0x00, 0x00, /* data = stream_id (1) */
        };
        status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), &out);
        TEST_ASSERT(status == STATUS_SUCCESS);
        TEST_ASSERT(out.Kind == VIRTIO_SND_EVENT_KIND_PCM_XRUN);
        TEST_ASSERT_EQ_U32(out.Type, VIRTIO_SND_EVT_PCM_XRUN);
        TEST_ASSERT_EQ_U32(out.Data, 1u);
        TEST_ASSERT_EQ_U32(out.u.StreamId, 1u);
    }

    /* CTL_NOTIFY */
    {
        const uint8_t buf[] = {
            0x00, 0x12, 0x00, 0x00, /* type */
            0x2A, 0x00, 0x00, 0x00, /* data = control_id (42) */
        };
        status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), &out);
        TEST_ASSERT(status == STATUS_SUCCESS);
        TEST_ASSERT(out.Kind == VIRTIO_SND_EVENT_KIND_CTL_NOTIFY);
        TEST_ASSERT_EQ_U32(out.Type, VIRTIO_SND_EVT_CTL_NOTIFY);
        TEST_ASSERT_EQ_U32(out.Data, 42u);
        TEST_ASSERT_EQ_U32(out.u.CtlId, 42u);
    }
}

static void test_parse_trailing_bytes_are_ignored(void)
{
    VIRTIO_SND_EVENT_PARSED out;
    NTSTATUS status;

    /*
     * Devices may legally complete event buffers with extra trailing bytes.
     * The parser only inspects the fixed-size header.
     */
    {
        const uint8_t buf[] = {
            0x00, 0x11, 0x00, 0x00, /* type = PCM_PERIOD_ELAPSED */
            0x02, 0x00, 0x00, 0x00, /* data = 2 */
            0xAA, 0xBB, 0xCC, 0xDD, /* extra bytes */
        };
        status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), &out);
        TEST_ASSERT(status == STATUS_SUCCESS);
        TEST_ASSERT(out.Kind == VIRTIO_SND_EVENT_KIND_PCM_PERIOD_ELAPSED);
        TEST_ASSERT_EQ_U32(out.Type, VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED);
        TEST_ASSERT_EQ_U32(out.Data, 2u);
    }
}

static void test_parse_short_buffers_are_rejected_safely(void)
{
    VIRTIO_SND_EVENT_PARSED out;
    NTSTATUS status;

    {
        const uint8_t buf[] = {0};
        status = VirtioSndParseEvent(buf, 0u, &out);
        TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);
    }

    {
        const uint8_t buf[] = {0, 1, 2, 3, 4, 5, 6};
        status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), &out);
        TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);
    }
}

static void test_parse_unknown_event_is_tolerated(void)
{
    VIRTIO_SND_EVENT_PARSED out;
    NTSTATUS status;

    const uint8_t buf[] = {
        0xEF, 0xBE, 0xAD, 0xDE, /* type = 0xDEADBEEF */
        0x01, 0x02, 0x03, 0x04, /* data */
    };

    status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), &out);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(out.Kind == VIRTIO_SND_EVENT_KIND_UNKNOWN);
    TEST_ASSERT_EQ_U32(out.Type, 0xDEADBEEFu);
    TEST_ASSERT_EQ_U32(out.Data, 0x04030201u);
    /* Unknown events must not leak stale data into the typed union. */
    TEST_ASSERT_EQ_U32(out.u.JackId, 0u);
}

static void test_parse_rejects_invalid_parameters(void)
{
    VIRTIO_SND_EVENT_PARSED out;
    NTSTATUS status;

    {
        const uint8_t buf[] = {
            0x00, 0x11, 0x00, 0x00, /* type */
            0x00, 0x00, 0x00, 0x00, /* data */
        };
        status = VirtioSndParseEvent(NULL, (ULONG)sizeof(buf), &out);
        TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

        status = VirtioSndParseEvent(buf, (ULONG)sizeof(buf), NULL);
        TEST_ASSERT(status == STATUS_INVALID_PARAMETER);
    }
}

static void test_parse_unaligned_buffer(void)
{
    uint8_t raw[1 + sizeof(VIRTIO_SND_EVENT)];
    uint8_t* buf = raw + 1;
    VIRTIO_SND_EVENT_PARSED out;
    NTSTATUS status;

    RtlZeroMemory(raw, sizeof(raw));

    buf[0] = 0x00;
    buf[1] = 0x11;
    buf[2] = 0x00;
    buf[3] = 0x00; /* PCM_PERIOD_ELAPSED */
    buf[4] = 0x01;
    buf[5] = 0x00;
    buf[6] = 0x00;
    buf[7] = 0x00; /* data = 1 */

    status = VirtioSndParseEvent(buf, (ULONG)sizeof(VIRTIO_SND_EVENT), &out);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(out.Kind == VIRTIO_SND_EVENT_KIND_PCM_PERIOD_ELAPSED);
    TEST_ASSERT_EQ_U32(out.Type, VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED);
    TEST_ASSERT_EQ_U32(out.Data, 1u);
    TEST_ASSERT_EQ_U32(out.u.StreamId, 1u);
}

int main(void)
{
    test_event_struct_packing_and_endianness();
    test_parse_known_event_types();
    test_parse_trailing_bytes_are_ignored();
    test_parse_short_buffers_are_rejected_safely();
    test_parse_unknown_event_is_tolerated();
    test_parse_rejects_invalid_parameters();
    test_parse_unaligned_buffer();

    printf("virtiosnd_event_proto_tests: PASS\n");
    return 0;
}
