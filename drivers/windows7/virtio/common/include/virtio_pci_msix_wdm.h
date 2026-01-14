/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * WDM helper for virtio-pci modern MSI/MSI-X interrupts.
 *
 * Virtio modern devices can expose message-signaled interrupts (MSI or MSI-X).
 * Windows surfaces these as CmResourceTypeInterrupt descriptors with
 * CM_RESOURCE_INTERRUPT_MESSAGE set and a MessageCount.
 *
 * This helper provides a reusable message-interrupt ISR + per-vector DPC layer
 * for WDM drivers (no WDF/KMDF dependencies). It implements the same vector
 * mapping policy as the shared KMDF helper:
 *
 *   - Vector 0 handles configuration change notifications.
 *   - If MessageCount >= (1 + QueueCount):
 *       vectors 1..QueueCount map to queues 0..QueueCount-1 respectively.
 *     Else:
 *       vector 0 drains all queues (single-vector fallback).
 *
 * Concurrency notes:
 *   - Message-interrupt DPCs may execute concurrently on different CPUs.
 *   - Queue draining is serialized by per-queue KSPIN_LOCKs allocated by this
 *     helper in nonpaged memory.
 *   - Any caller code sequence that writes common_cfg.queue_select and then
 *     accesses queue-specific fields MUST be globally serialized. Callers may
 *     provide a CommonCfgLock pointer for this purpose (the helper stores it,
 *     but does not acquire it implicitly).
 */

#pragma once

#include <ntddk.h>

#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
#define CM_RESOURCE_INTERRUPT_MESSAGE 0x0004
#endif

#ifndef CONNECT_MESSAGE_BASED
/*
 * Some older WDK header sets omit the CONNECT_MESSAGE_BASED definition even
 * though IoConnectInterruptEx supports message-based interrupts on Vista+.
 *
 * The documented value is 2.
 */
#define CONNECT_MESSAGE_BASED 0x2
#endif

#ifndef DISCONNECT_MESSAGE_BASED
/* Some WDKs use DISCONNECT_MESSAGE_BASED for IoDisconnectInterruptEx; others reuse CONNECT_MESSAGE_BASED. */
#define DISCONNECT_MESSAGE_BASED CONNECT_MESSAGE_BASED
#endif

/* Virtio spec sentinel for "no MSI-X vector assigned". */
#ifndef VIRTIO_PCI_MSI_NO_VECTOR
#define VIRTIO_PCI_MSI_NO_VECTOR ((USHORT)0xFFFF)
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct _VIRTIO_MSIX_WDM VIRTIO_MSIX_WDM, *PVIRTIO_MSIX_WDM;

typedef VOID EVT_VIRTIO_MSIX_CONFIG_CHANGE(_In_ PDEVICE_OBJECT DeviceObject, _In_opt_ PVOID Cookie);

typedef VOID EVT_VIRTIO_MSIX_DRAIN_QUEUE(
    _In_ PDEVICE_OBJECT DeviceObject,
    _In_ ULONG QueueIndex,
    _In_opt_ PVOID Cookie);

typedef struct _VIRTIO_MSIX_WDM_VECTOR {
    KDPC Dpc;

    /* Message vector index (0-based). */
    USHORT VectorIndex;

    /* TRUE iff this vector should invoke EvtConfigChange. */
    BOOLEAN HandlesConfig;

    /* Bitmask of queues to drain for this vector. */
    ULONGLONG QueueMask;

    /* Back-pointer to the parent helper state. */
    PVIRTIO_MSIX_WDM Msix;
} VIRTIO_MSIX_WDM_VECTOR, *PVIRTIO_MSIX_WDM_VECTOR;

typedef struct _VIRTIO_MSIX_WDM {
    /*
     * Device object passed to callbacks (typically the FDO).
     *
     * Note: this is distinct from the PhysicalDeviceObject (PDO) required by
     * IoConnectInterruptEx for message-based interrupts.
     */
    PDEVICE_OBJECT DeviceObject;

    /* Physical device object (PDO) used for IoConnectInterruptEx. */
    PDEVICE_OBJECT PhysicalDeviceObject;

    ULONG QueueCount;

    /* Optional global lock used by callers to serialize `queue_select` sequences. */
    PKSPIN_LOCK CommonCfgLock;

    /* DPC callbacks (all optional). */
    EVT_VIRTIO_MSIX_CONFIG_CHANGE* EvtConfigChange;
    EVT_VIRTIO_MSIX_DRAIN_QUEUE* EvtDrainQueue;
    PVOID Cookie;

    /* Total messages available per translated resource descriptor. */
    ULONG MessageCount;

    /*
     * Number of vectors actually connected/used by this helper.
     * (1 for single-vector fallback, or 1 + QueueCount for multi-vector mode.)
     */
    USHORT UsedVectorCount;

    /*
     * Message numbers (MSI-X table entry indices) to program into the virtio
     * common_cfg routing fields.
     *
     * - ConfigVector is for common_cfg.msix_config
     * - QueueVectors[q] is for common_cfg.queue_msix_vector for queue q
     *
     * When UsedVectorCount == 1, all queues share ConfigVector.
     */
    USHORT ConfigVector;
    USHORT* QueueVectors; /* length QueueCount, allocated by this helper */

    /*
     * Message interrupt connection returned by IoConnectInterruptEx.
     * The helper stores these to support triggering in unit tests and to
     * disconnect cleanly.
     */
    PIO_INTERRUPT_MESSAGE_INFO MessageInfo;
    PVOID ConnectionContext;

    /* Per-vector DPC state (length UsedVectorCount, allocated by this helper). */
    VIRTIO_MSIX_WDM_VECTOR* Vectors;

    /* Per-queue locks (length QueueCount, allocated by this helper). */
    KSPIN_LOCK* QueueLocks;

    /* Tracks queued + running DPC instances across all vectors. */
    volatile LONG DpcInFlight;

    /* Internal: set TRUE by VirtioMsixConnect after DPC initialization. */
    BOOLEAN Initialized;
} VIRTIO_MSIX_WDM;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioMsixConnect(_In_ PDEVICE_OBJECT DeviceObject,
                           _In_ PDEVICE_OBJECT PhysicalDeviceObject,
                           _In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptDescTranslated,
                           _In_ ULONG QueueCount,
                           _In_opt_ PKSPIN_LOCK CommonCfgLock,
                           _In_opt_ EVT_VIRTIO_MSIX_CONFIG_CHANGE* EvtConfigChange,
                           _In_opt_ EVT_VIRTIO_MSIX_DRAIN_QUEUE* EvtDrainQueue,
                           _In_opt_ PVOID Cookie,
                           _Inout_ PVIRTIO_MSIX_WDM Msix);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID VirtioMsixDisconnect(_Inout_ PVIRTIO_MSIX_WDM Msix);

#ifdef __cplusplus
} /* extern "C" */
#endif
