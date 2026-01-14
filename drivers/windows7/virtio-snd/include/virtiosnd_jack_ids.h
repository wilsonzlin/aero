/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

/*
 * Shared jack ID definitions for the Windows 7 virtio-snd driver.
 *
 * Keep this header free of WDK/PortCls dependencies so it can be included from
 * host-buildable modules (unit tests) as well as kernel-mode PortCls miniports.
 */

#define VIRTIOSND_JACK_ID_SPEAKER     0u
#define VIRTIOSND_JACK_ID_MICROPHONE  1u
#define VIRTIOSND_JACK_ID_COUNT       2u

