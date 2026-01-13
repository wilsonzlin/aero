/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtio_snd_proto.h"
#include "virtiosnd_jack.h"

enum {
    TEST_JACK_ID_SPEAKER = 0u,
    TEST_JACK_ID_MICROPHONE = 1u,
};

static void test_jack_state_defaults_to_connected(void)
{
    VIRTIOSND_JACK_STATE state;

    VirtIoSndJackStateInit(&state);

    TEST_ASSERT(VirtIoSndJackStateIsConnected(&state, TEST_JACK_ID_SPEAKER) == TRUE);
    TEST_ASSERT(VirtIoSndJackStateIsConnected(&state, TEST_JACK_ID_MICROPHONE) == TRUE);
}

static void test_jack_disconnect_and_connect_transitions(void)
{
    VIRTIOSND_JACK_STATE state;
    VIRTIO_SND_EVENT evt;
    ULONG jackId;
    BOOLEAN connected;
    BOOLEAN changed;

    VirtIoSndJackStateInit(&state);

    /* Disconnect speaker jack. */
    evt.type = VIRTIO_SND_EVT_JACK_DISCONNECTED;
    evt.data = TEST_JACK_ID_SPEAKER;
    jackId = 0;
    connected = TRUE;
    changed = VirtIoSndJackStateProcessEventqBuffer(&state, &evt, sizeof(evt), &jackId, &connected);
    TEST_ASSERT(changed == TRUE);
    TEST_ASSERT(jackId == TEST_JACK_ID_SPEAKER);
    TEST_ASSERT(connected == FALSE);
    TEST_ASSERT(VirtIoSndJackStateIsConnected(&state, TEST_JACK_ID_SPEAKER) == FALSE);

    /* Re-sending the same state should not report a change. */
    changed = VirtIoSndJackStateProcessEventqBuffer(&state, &evt, sizeof(evt), NULL, NULL);
    TEST_ASSERT(changed == FALSE);

    /* Connect speaker jack again. */
    evt.type = VIRTIO_SND_EVT_JACK_CONNECTED;
    evt.data = TEST_JACK_ID_SPEAKER;
    changed = VirtIoSndJackStateProcessEventqBuffer(&state, &evt, sizeof(evt), &jackId, &connected);
    TEST_ASSERT(changed == TRUE);
    TEST_ASSERT(jackId == TEST_JACK_ID_SPEAKER);
    TEST_ASSERT(connected == TRUE);
    TEST_ASSERT(VirtIoSndJackStateIsConnected(&state, TEST_JACK_ID_SPEAKER) == TRUE);
}

static void test_unknown_event_is_ignored(void)
{
    VIRTIOSND_JACK_STATE state;
    VIRTIO_SND_EVENT evt;
    BOOLEAN changed;

    VirtIoSndJackStateInit(&state);

    evt.type = 0xDEADBEEFu;
    evt.data = TEST_JACK_ID_SPEAKER;
    changed = VirtIoSndJackStateProcessEventqBuffer(&state, &evt, sizeof(evt), NULL, NULL);
    TEST_ASSERT(changed == FALSE);
    TEST_ASSERT(VirtIoSndJackStateIsConnected(&state, TEST_JACK_ID_SPEAKER) == TRUE);
}

static void test_unknown_jack_id_is_ignored(void)
{
    VIRTIOSND_JACK_STATE state;
    VIRTIO_SND_EVENT evt;
    BOOLEAN changed;

    VirtIoSndJackStateInit(&state);

    evt.type = VIRTIO_SND_EVT_JACK_DISCONNECTED;
    evt.data = 99u;
    changed = VirtIoSndJackStateProcessEventqBuffer(&state, &evt, sizeof(evt), NULL, NULL);
    TEST_ASSERT(changed == FALSE);
    TEST_ASSERT(VirtIoSndJackStateIsConnected(&state, TEST_JACK_ID_SPEAKER) == TRUE);
}

static void test_short_used_len_is_ignored(void)
{
    VIRTIOSND_JACK_STATE state;
    UCHAR buf[4];
    BOOLEAN changed;

    VirtIoSndJackStateInit(&state);
    memset(buf, 0, sizeof(buf));

    changed = VirtIoSndJackStateProcessEventqBuffer(&state, buf, (UINT32)sizeof(buf), NULL, NULL);
    TEST_ASSERT(changed == FALSE);
}

int main(void)
{
    test_jack_state_defaults_to_connected();
    test_jack_disconnect_and_connect_transitions();
    test_unknown_event_is_ignored();
    test_unknown_jack_id_is_ignored();
    test_short_used_len_is_ignored();

    printf("virtiosnd_jack_tests: PASS\n");
    return 0;
}
