/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Virtio PCI INTx helpers for Windows 7 WDM drivers.
 *
 * Contract v1 requires INTx and uses the virtio ISR status register
 * (VIRTIO_PCI_CAP_ISR_CFG) as a read-to-ack mechanism to deassert the line.
 */

#pragma once

#include <ntddk.h>

/* ISR status bits (read-to-ack). */
#ifndef VIRTIO_PCI_ISR_QUEUE
#define VIRTIO_PCI_ISR_QUEUE 0x01u
#endif
#ifndef VIRTIO_PCI_ISR_CONFIG
#define VIRTIO_PCI_ISR_CONFIG 0x02u
#endif

typedef VOID EVT_VIRTIO_INTX_WDM_QUEUE_DPC(_In_opt_ PVOID Context);
typedef VOID EVT_VIRTIO_INTX_WDM_CONFIG_DPC(_In_opt_ PVOID Context);

typedef struct _VIRTIO_INTX_WDM {
    PKINTERRUPT InterruptObject;
    KDPC Dpc;
    KEVENT DpcIdleEvent;

    volatile UCHAR *IsrStatus; /* MMIO: read-to-ack */
    volatile LONG PendingIsrStatus;

    /*
     * Teardown coordination:
     * - Stopping prevents the ISR/DPC from calling back into the driver while
     *   resources are being freed.
     * - DpcInFlight tracks both queued and running DPC instances so Disconnect
     *   can safely wait even if the KDPC is re-queued while executing.
     */
    volatile LONG Stopping;
    volatile LONG DpcInFlight;

    volatile LONG IsrCount;
    volatile LONG DpcCount;
    volatile UCHAR LastIsrStatus;

    EVT_VIRTIO_INTX_WDM_QUEUE_DPC *EvtQueueDpc;
    PVOID EvtQueueDpcContext;

    EVT_VIRTIO_INTX_WDM_CONFIG_DPC *EvtConfigDpc;
    PVOID EvtConfigDpcContext;
} VIRTIO_INTX_WDM;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioIntxConnect(_In_ PDEVICE_OBJECT DeviceObject,
                  _In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR *InterruptDescTranslated,
                  _In_ volatile UCHAR *IsrStatusMmio,
                  _In_opt_ EVT_VIRTIO_INTX_WDM_QUEUE_DPC *EvtQueueDpc,
                  _In_opt_ PVOID EvtQueueDpcContext,
                  _In_opt_ EVT_VIRTIO_INTX_WDM_CONFIG_DPC *EvtConfigDpc,
                  _In_opt_ PVOID EvtConfigDpcContext,
                  _Out_ VIRTIO_INTX_WDM *Intx);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioIntxDisconnect(_Inout_ VIRTIO_INTX_WDM *Intx);
