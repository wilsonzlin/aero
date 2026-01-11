/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * WDM helper for virtio-pci modern INTx interrupts.
 *
 * The virtio-pci ISR status register (VIRTIO_PCI_CAP_ISR_CFG) is a single byte
 * read-to-clear register. For INTx (level-triggered, often shared), reading this
 * byte is the acknowledge/deassert operation.
 *
 * This helper provides a reusable ISR + DPC pair that:
 *   - ACKs/deasserts INTx by reading the ISR status byte in the ISR (first MMIO op)
 *   - accumulates ISR bits between ISR and DPC
 *   - dispatches config/queue work in a DPC at DISPATCH_LEVEL
 *
 * Framework-agnostic WDM (no WDF/KMDF dependencies).
 */

#pragma once

#include <ntddk.h>

#ifdef __cplusplus
extern "C" {
#endif

#define VIRTIO_PCI_ISR_QUEUE_INTERRUPT  0x01
#define VIRTIO_PCI_ISR_CONFIG_INTERRUPT 0x02

typedef struct _VIRTIO_INTX VIRTIO_INTX, *PVIRTIO_INTX;

typedef VOID EVT_VIRTIO_INTX_CONFIG_CHANGE(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie);
typedef VOID EVT_VIRTIO_INTX_QUEUE_WORK(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie);

/*
 * Optional single-dispatch callback invoked in the DPC with the latched ISR byte.
 *
 * If supplied, this callback is responsible for interpreting IsrStatus bits and
 * performing any required work. If NULL, the helper will invoke EvtConfigChange
 * and/or EvtQueueWork based on bits 1/0 respectively.
 */
typedef VOID EVT_VIRTIO_INTX_DPC(_Inout_ PVIRTIO_INTX Intx, _In_ UCHAR IsrStatus, _In_opt_ PVOID Cookie);

typedef struct _VIRTIO_INTX {
    /* Interrupt object from IoConnectInterrupt. */
    PKINTERRUPT InterruptObject;

    /* KDPC queued from the ISR. */
    KDPC Dpc;

    /* Mapped virtio ISR status register (read-to-clear). */
    volatile UCHAR* IsrStatusRegister;

    /* Latched ISR status bits accumulated between ISR and DPC. */
    volatile LONG PendingIsrStatus;

    /*
     * Tracks queued + running DPC instances so teardown can safely wait even if
     * the KDPC is re-queued while executing.
     */
    volatile LONG DpcInFlight;

    /* Optional diagnostic counters (incremented via interlocked ops). */
    volatile LONG IsrCount;
    volatile LONG SpuriousCount;
    volatile LONG DpcCount;

    /* DPC callbacks (all optional). */
    EVT_VIRTIO_INTX_CONFIG_CHANGE* EvtConfigChange;
    EVT_VIRTIO_INTX_QUEUE_WORK* EvtQueueWork;
    EVT_VIRTIO_INTX_DPC* EvtDpc;
    PVOID Cookie;

    /* Internal: set TRUE by VirtioIntxConnect after DPC initialization. */
    BOOLEAN Initialized;
} VIRTIO_INTX;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioIntxConnect(_In_ PDEVICE_OBJECT DeviceObject,
                           _In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptDescTranslated,
                           _In_opt_ volatile UCHAR* IsrStatusRegister,
                           _In_opt_ EVT_VIRTIO_INTX_CONFIG_CHANGE* EvtConfigChange,
                           _In_opt_ EVT_VIRTIO_INTX_QUEUE_WORK* EvtQueueWork,
                           _In_opt_ EVT_VIRTIO_INTX_DPC* EvtDpc,
                           _In_opt_ PVOID Cookie,
                           _Inout_ PVIRTIO_INTX Intx);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID VirtioIntxDisconnect(_Inout_ PVIRTIO_INTX Intx);

#ifdef __cplusplus
} /* extern "C" */
#endif

