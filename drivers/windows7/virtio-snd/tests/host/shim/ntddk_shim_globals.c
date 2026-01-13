/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

/*
 * Shared by the ntddk.h host shim so tests can simulate DISPATCH_LEVEL code
 * paths and hook event signaling.
 *
 * The canonical implementation lives in drivers/windows7/virtio-snd/tests/ntddk.h.
 */
volatile KIRQL g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
VIRTIOSND_TEST_KE_SET_EVENT_HOOK g_virtiosnd_test_ke_set_event_hook = NULL;

