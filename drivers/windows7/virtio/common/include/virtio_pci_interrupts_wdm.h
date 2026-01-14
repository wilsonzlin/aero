/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * WDM helper for virtio-pci modern interrupts (INTx + MSI/MSI-X).
 *
 * INTx:
 *   - Uses virtio ISR status byte as read-to-clear ACK/deassert (first MMIO op).
 *   - Returns FALSE if the ISR status byte reads as 0 (spurious/shared interrupt).
 *   - Implemented by reusing virtio_pci_intx_wdm.
 *
 * MSI/MSI-X (message-signaled):
 *   - Connects message-based interrupts with IoConnectInterruptEx.
 *   - ISR must NOT read the virtio ISR status byte (routing is via MessageId).
 *   - Dispatches work in a per-message KDPC at DISPATCH_LEVEL.
 *
 * This helper is framework-agnostic WDM (no WDF/KMDF dependencies).
 */

#pragma once

#include <ntddk.h>

#include "virtio_pci_intx_wdm.h"

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

#ifdef __cplusplus
extern "C" {
#endif

/* Sentinel used by the helper when a queue index is not known (INTx). */
#define VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN ((USHORT)0xFFFF)

/* Sentinel used when an interrupt indicates no queue work (config-only). */
#define VIRTIO_PCI_WDM_QUEUE_INDEX_NONE ((USHORT)0xFFFE)

/* Sentinel used by the helper when there is no message ID (INTx). */
#define VIRTIO_PCI_WDM_MESSAGE_ID_NONE ((ULONG)0xFFFFFFFFu)

typedef enum _VIRTIO_PCI_WDM_INTERRUPT_MODE {
    VirtioPciWdmInterruptModeUnknown = 0,
    VirtioPciWdmInterruptModeIntx,
    VirtioPciWdmInterruptModeMessage,
} VIRTIO_PCI_WDM_INTERRUPT_MODE;

typedef struct _VIRTIO_PCI_WDM_MESSAGE_ROUTE {
    BOOLEAN IsConfig;
    /*
     * QueueIndex routing for MSI/MSI-X.
     *
     * - VIRTIO_PCI_WDM_QUEUE_INDEX_NONE: config-only (no queue work)
     * - VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN: queue work without a specific queue (e.g. all queues / INTx-like)
     * - otherwise: specific virtqueue index.
     */
    USHORT QueueIndex;
} VIRTIO_PCI_WDM_MESSAGE_ROUTE;

typedef struct _VIRTIO_PCI_WDM_INTERRUPTS VIRTIO_PCI_WDM_INTERRUPTS, *PVIRTIO_PCI_WDM_INTERRUPTS;

typedef VOID EVT_VIRTIO_PCI_WDM_CONFIG_CHANGE(_Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts, _In_opt_ PVOID Cookie);

typedef VOID EVT_VIRTIO_PCI_WDM_QUEUE_WORK(
    _Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts,
    _In_ USHORT QueueIndex,
    _In_opt_ PVOID Cookie);

/*
 * Optional single-dispatch callback invoked in the DPC for each interrupt cause.
 *
 * INTx: invoked once for config and/or queue depending on ISR bits, with:
 *   - MessageId = VIRTIO_PCI_WDM_MESSAGE_ID_NONE
 *   - QueueIndex = VIRTIO_PCI_WDM_QUEUE_INDEX_NONE for config-only dispatch
 *   - QueueIndex = VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN for queue work
 *
 * MSI/MSI-X: invoked once or twice per message interrupt, depending on routing:
 *   - config dispatch: IsConfig=TRUE, QueueIndex=VIRTIO_PCI_WDM_QUEUE_INDEX_NONE
 *   - optional queue dispatch: IsConfig=FALSE, QueueIndex per routing table
 */
typedef VOID EVT_VIRTIO_PCI_WDM_DPC(
    _Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts,
    _In_ ULONG MessageId,
    _In_ BOOLEAN IsConfig,
    _In_ USHORT QueueIndex,
    _In_opt_ PVOID Cookie);

typedef struct _VIRTIO_PCI_WDM_INTERRUPTS {
    VIRTIO_PCI_WDM_INTERRUPT_MODE Mode;

    /* Callbacks (all optional). */
    EVT_VIRTIO_PCI_WDM_CONFIG_CHANGE* EvtConfigChange;
    EVT_VIRTIO_PCI_WDM_QUEUE_WORK* EvtQueueWork;
    EVT_VIRTIO_PCI_WDM_DPC* EvtDpc;
    PVOID Cookie;

    union {
        struct {
            VIRTIO_INTX Intx;
        } Intx;

        struct {
            /* Opaque connection context returned by IoConnectInterruptEx. */
            PVOID ConnectionContext;

            /* IoConnectInterruptEx(CONNECT_MESSAGE_BASED) output describing connected messages. */
            PIO_INTERRUPT_MESSAGE_INFO MessageInfo;

            ULONG MessageCount;

            /* Arrays allocated from NonPagedPool: [MessageCount]. */
            PKDPC MessageDpcs;
            VIRTIO_PCI_WDM_MESSAGE_ROUTE* Routes;

            volatile LONG DpcInFlight;
            volatile LONG IsrCount;
            volatile LONG DpcCount;
        } Message;
    } u;

    /* Internal: set TRUE by VirtioPciWdmInterruptConnect after initialization. */
    BOOLEAN Initialized;
} VIRTIO_PCI_WDM_INTERRUPTS;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciWdmInterruptConnect(
    /*
     * Device object for this driver (typically the FDO).
     *
     * Note: For message-signaled interrupts, IoConnectInterruptEx requires a
     * PhysicalDeviceObject (PDO) which may differ from the FDO. Callers must
     * provide both.
     */
    _In_ PDEVICE_OBJECT DeviceObject,
    /* PDO required for IoConnectInterruptEx(CONNECT_MESSAGE_BASED). */
    _In_opt_ PDEVICE_OBJECT PhysicalDeviceObject,
    _In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptDescTranslated,
    _In_opt_ volatile UCHAR* IsrStatusRegister,
    _In_opt_ EVT_VIRTIO_PCI_WDM_CONFIG_CHANGE* EvtConfigChange,
    _In_opt_ EVT_VIRTIO_PCI_WDM_QUEUE_WORK* EvtQueueWork,
    _In_opt_ EVT_VIRTIO_PCI_WDM_DPC* EvtDpc,
    _In_opt_ PVOID Cookie,
    _Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID VirtioPciWdmInterruptDisconnect(_Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts);

/*
 * Updates the message routing table for MSI/MSI-X.
 *
 * Safe to call only while interrupts are quiesced (typically during device start/reset)
 * so DPCs cannot race with table updates.
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciWdmInterruptSetMessageRoute(
    _Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts,
    _In_ ULONG MessageId,
    _In_ BOOLEAN IsConfig,
    _In_ USHORT QueueIndex);

#ifdef __cplusplus
} /* extern "C" */
#endif
