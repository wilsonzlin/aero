/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtio_pci_modern_wdm.h"
#include "virtiosnd_control.h"
#include "virtiosnd_queue_split.h"
#include "virtiosnd_tx.h"

#define VIRTIOSND_POOL_TAG 'dnSV' // 'VSnd' (endianness depends on debugger display)

/* The Aero contract requires three mandatory virtqueues (control/event/tx). */
#define VIRTIOSND_QUEUE_CONTROL VIRTIO_SND_QUEUE_CONTROL
#define VIRTIOSND_QUEUE_EVENT VIRTIO_SND_QUEUE_EVENT
#define VIRTIOSND_QUEUE_TX VIRTIO_SND_QUEUE_TX
#define VIRTIOSND_QUEUE_COUNT 3u

typedef struct _VIRTIOSND_DEVICE_EXTENSION {
    PDEVICE_OBJECT Self;
    PDEVICE_OBJECT Pdo;
    PDEVICE_OBJECT LowerDeviceObject;

    IO_REMOVE_LOCK RemoveLock;

    VIRTIOSND_TRANSPORT Transport;
    UINT64 NegotiatedFeatures;

    /*
     * Split virtqueue rings + queue abstractions.
     *
     * QueueSplit[] owns the DMA memory and VIRTQ_SPLIT state.
     * Queues[] provides a minimal Submit/PopUsed/Kick API used by future
     * virtio-snd protocol code.
     */
    VIRTIOSND_QUEUE_SPLIT QueueSplit[VIRTIOSND_QUEUE_COUNT];
    VIRTIOSND_QUEUE Queues[VIRTIOSND_QUEUE_COUNT];

    /* Protocol engines (controlq + txq) */
    VIRTIOSND_CONTROL Control;
    VIRTIOSND_TX_ENGINE Tx;

    /* INTx plumbing */
    PKINTERRUPT InterruptObject;
    KDPC InterruptDpc;
    volatile LONG PendingIsrStatus;
    volatile LONG Stopping;
    volatile LONG DpcInFlight;
    KEVENT DpcIdleEvent;

    ULONG InterruptVector;
    KIRQL InterruptIrql;
    KINTERRUPT_MODE InterruptMode;
    KAFFINITY InterruptAffinity;
    BOOLEAN InterruptShareVector;

    VIRTIOSND_DMA_CONTEXT DmaCtx;

    BOOLEAN Started;
    BOOLEAN Removed;
} VIRTIOSND_DEVICE_EXTENSION, *PVIRTIOSND_DEVICE_EXTENSION;

#define VIRTIOSND_GET_DX(_DeviceObject) ((PVIRTIOSND_DEVICE_EXTENSION)(_DeviceObject)->DeviceExtension)

NTSTATUS
VirtIoSndStartHardware(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_opt_ PCM_RESOURCE_LIST RawResources,
    _In_opt_ PCM_RESOURCE_LIST TranslatedResources
    );

VOID
VirtIoSndStopHardware(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);
