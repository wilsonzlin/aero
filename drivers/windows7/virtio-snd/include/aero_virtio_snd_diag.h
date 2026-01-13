/* SPDX-License-Identifier: MIT OR Apache-2.0 */
#pragma once

/*
 * User-mode accessible diagnostics interface for aero_virtio_snd.sys.
 *
 * The audio/PortCls stack does not expose a convenient control device for
 * simple diagnostics. For testability the driver may (best-effort) create a
 * separate device object named:
 *   \\.\aero_virtio_snd_diag
 *
 * This interface is optional: driver functionality must not depend on it.
 */

/* Keep this header buildable in both kernel and user-mode environments. */
#if defined(_KERNEL_MODE)
#include <ntddk.h>
#else
#include <windows.h>
#include <winioctl.h>
#endif

/* Fixed virtio-snd queue count under the Aero contract v1. */
#define AERO_VIRTIO_SND_DIAG_QUEUE_COUNT 4u

enum {
    AERO_VIRTIO_SND_DIAG_IRQ_MODE_NONE = 0,
    AERO_VIRTIO_SND_DIAG_IRQ_MODE_INTX = 1,
    AERO_VIRTIO_SND_DIAG_IRQ_MODE_MSIX = 2,
};

/*
 * IOCTL interface.
 *
 * The interface is versioned; callers must set Size/Version to known values
 * and should tolerate the driver returning a larger Size (future extension).
 */
#define AERO_VIRTIO_SND_DIAG_VERSION 1u

typedef struct _AERO_VIRTIO_SND_DIAG_INFO {
    ULONG Size;     /* sizeof(AERO_VIRTIO_SND_DIAG_INFO) */
    ULONG Version;  /* AERO_VIRTIO_SND_DIAG_VERSION */

    ULONG IrqMode;      /* AERO_VIRTIO_SND_DIAG_IRQ_MODE_* */
    ULONG MessageCount; /* MSI/MSI-X messages granted by the OS (0 in INTx mode) */

    USHORT MsixConfigVector;
    USHORT Reserved0;
    USHORT QueueMsixVector[AERO_VIRTIO_SND_DIAG_QUEUE_COUNT];
    USHORT Reserved1;

    ULONG InterruptCount;
    ULONG DpcCount;
    ULONG QueueDrainCount[AERO_VIRTIO_SND_DIAG_QUEUE_COUNT];
} AERO_VIRTIO_SND_DIAG_INFO, *PAERO_VIRTIO_SND_DIAG_INFO;

/* Query current interrupt mode/statistics. */
#define IOCTL_AERO_VIRTIO_SND_DIAG_QUERY \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0xA01u, METHOD_BUFFERED, FILE_READ_ACCESS)
