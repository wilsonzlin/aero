/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtio_pci_modern_wdm.h"
#include "virtiosnd_control.h"
#include "virtiosnd_queue_split.h"
#include "virtiosnd_tx.h"

#define VIRTIOSND_POOL_TAG 'dnSV' // 'VSnd' (endianness depends on debugger display)
#define VIRTIOSND_DX_SIGNATURE 'xdSV'

//
// PortCls subdevice names (must match the driver's PcRegisterSubdevice names).
//
#define VIRTIOSND_SUBDEVICE_WAVE L"Wave"
#define VIRTIOSND_SUBDEVICE_TOPOLOGY L"Topology"

//
// Miniport pin IDs.
//
#define VIRTIOSND_WAVE_PIN_RENDER 0
#define VIRTIOSND_WAVE_PIN_BRIDGE 1

#define VIRTIOSND_TOPO_PIN_BRIDGE 0
#define VIRTIOSND_TOPO_PIN_SPEAKER 1

//
// Fixed audio format: 48kHz, 2ch, 16-bit PCM LE.
//
#define VIRTIOSND_SAMPLE_RATE 48000
#define VIRTIOSND_CHANNELS 2
#define VIRTIOSND_BITS_PER_SAMPLE 16
#define VIRTIOSND_BYTES_PER_SAMPLE (VIRTIOSND_BITS_PER_SAMPLE / 8)
#define VIRTIOSND_BLOCK_ALIGN (VIRTIOSND_CHANNELS * VIRTIOSND_BYTES_PER_SAMPLE)
#define VIRTIOSND_AVG_BYTES_PER_SEC (VIRTIOSND_SAMPLE_RATE * VIRTIOSND_BLOCK_ALIGN)

//
// Fixed timer period (10ms).
//
#define VIRTIOSND_PERIOD_FRAMES 480
#define VIRTIOSND_PERIOD_BYTES (VIRTIOSND_PERIOD_FRAMES * VIRTIOSND_BLOCK_ALIGN)

/*
 * The Aero contract defines four virtqueues (control/event/tx/rx).
 *
 * This driver currently implements controlq + txq for playback. eventq and rxq
 * are brought up for transport bring-up but do not have protocol engines yet
 * (rxq capture buffers are not submitted yet).
 */
#define VIRTIOSND_QUEUE_CONTROL VIRTIO_SND_QUEUE_CONTROL
#define VIRTIOSND_QUEUE_EVENT VIRTIO_SND_QUEUE_EVENT
#define VIRTIOSND_QUEUE_TX VIRTIO_SND_QUEUE_TX
#define VIRTIOSND_QUEUE_RX VIRTIO_SND_QUEUE_RX
#define VIRTIOSND_QUEUE_COUNT 4u

typedef struct _VIRTIOSND_DEVICE_EXTENSION {
    ULONG Signature;
    PDEVICE_OBJECT Self;
    PDEVICE_OBJECT Pdo;
    PDEVICE_OBJECT LowerDeviceObject;

    VIRTIOSND_TRANSPORT Transport;
    UINT64 NegotiatedFeatures;

    /*
     * Split virtqueue rings + queue abstractions.
     *
     * QueueSplit[] owns the DMA memory and VIRTQ_SPLIT state.
     * Queues[] provides a minimal Submit/PopUsed/Kick API used by higher-level
     * virtio-snd protocol code.
     */
    VIRTIOSND_QUEUE_SPLIT QueueSplit[VIRTIOSND_QUEUE_COUNT];
    VIRTIOSND_QUEUE Queues[VIRTIOSND_QUEUE_COUNT];

    /* Protocol engines (controlq + txq) */
    VIRTIOSND_CONTROL Control;
    VIRTIOSND_TX_ENGINE Tx;
    volatile LONG TxEngineInitialized;

    /* INTx plumbing */
    PKINTERRUPT InterruptObject;
    KDPC InterruptDpc;
    volatile LONG PendingIsrStatus;
    volatile LONG Stopping;
    volatile LONG DpcInFlight;

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

#ifdef __cplusplus
extern "C" {
#endif

NTSTATUS
VirtIoSndStartHardware(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_opt_ PCM_RESOURCE_LIST RawResources,
    _In_opt_ PCM_RESOURCE_LIST TranslatedResources
    );

VOID
VirtIoSndStopHardware(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/*
 * Hardware-facing protocol helpers intended for use by future PortCls/WaveRT
 * miniports.
 */

_IRQL_requires_max_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtIoSndHwSendControl(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_reads_bytes_(ReqLen) const void* Req,
    _In_ ULONG ReqLen,
    _Out_writes_bytes_(RespCap) void* Resp,
    _In_ ULONG RespCap,
    _In_ ULONG TimeoutMs,
    _Out_opt_ ULONG* OutVirtioStatus,
    _Out_opt_ ULONG* OutRespLen);

_IRQL_requires_max_(DISPATCH_LEVEL)
_Must_inspect_result_ NTSTATUS VirtIoSndHwSubmitTx(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_opt_ const VOID* Pcm1,
    _In_ ULONG Pcm1Bytes,
    _In_opt_ const VOID* Pcm2,
    _In_ ULONG Pcm2Bytes,
    _In_ BOOLEAN AllowSilenceFill);

/*
 * Submit a TX period as a list of DMA segments (no copy).
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
_Must_inspect_result_ NTSTATUS VirtIoSndHwSubmitTxSg(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_reads_(SegmentCount) const VIRTIOSND_TX_SEGMENT* Segments,
    _In_ ULONG SegmentCount);

/*
 * Drain used completions from the TX virtqueue and recycle TX contexts.
 *
 * This is useful for polling-driven use cases (when txq interrupts are
 * suppressed). The INTx DPC path drains completions automatically.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
ULONG VirtIoSndHwDrainTxCompletions(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

_IRQL_requires_max_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtIoSndInitTxEngine(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_ ULONG MaxPeriodBytes,
    _In_ ULONG BufferCount,
    _In_ BOOLEAN SuppressInterrupts);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID VirtIoSndUninitTxEngine(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

#ifdef __cplusplus
} /* extern "C" */
#endif
