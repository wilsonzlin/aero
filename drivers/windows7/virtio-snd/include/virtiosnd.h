/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtio_pci_intx_wdm.h"
#include "virtio_pci_modern_transport.h"
#include "virtiosnd_control.h"
#include "virtiosnd_eventq.h"
#include "virtiosnd_jack.h"
#include "virtiosnd_queue_split.h"
#include "virtiosnd_tx.h"
#include "virtiosnd_rx.h"

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
#define VIRTIOSND_WAVE_PIN_CAPTURE 2
#define VIRTIOSND_WAVE_PIN_BRIDGE_CAPTURE 3

#define VIRTIOSND_TOPO_PIN_BRIDGE 0
#define VIRTIOSND_TOPO_PIN_SPEAKER 1
#define VIRTIOSND_TOPO_PIN_BRIDGE_CAPTURE 2
#define VIRTIOSND_TOPO_PIN_MICROPHONE 3

//
// Baseline audio formats (Aero contract v1):
//  - Render (stream 0): 48kHz, stereo, 16-bit PCM LE
//  - Capture (stream 1): 48kHz, mono, 16-bit PCM LE
//
// Devices may advertise additional formats/rates via PCM_INFO; see the
// cached capability fields in VIRTIOSND_DEVICE_EXTENSION and the driver-supported
// subset in virtiosnd_control_proto.h.
//
#define VIRTIOSND_SAMPLE_RATE 48000
#define VIRTIOSND_CHANNELS 2
#define VIRTIOSND_BITS_PER_SAMPLE 16
#define VIRTIOSND_BYTES_PER_SAMPLE (VIRTIOSND_BITS_PER_SAMPLE / 8)
#define VIRTIOSND_BLOCK_ALIGN (VIRTIOSND_CHANNELS * VIRTIOSND_BYTES_PER_SAMPLE)
#define VIRTIOSND_AVG_BYTES_PER_SEC (VIRTIOSND_SAMPLE_RATE * VIRTIOSND_BLOCK_ALIGN)

#define VIRTIOSND_CAPTURE_CHANNELS 1
#define VIRTIOSND_CAPTURE_BLOCK_ALIGN (VIRTIOSND_CAPTURE_CHANNELS * VIRTIOSND_BYTES_PER_SAMPLE)
#define VIRTIOSND_CAPTURE_AVG_BYTES_PER_SEC (VIRTIOSND_SAMPLE_RATE * VIRTIOSND_CAPTURE_BLOCK_ALIGN)

//
// Default timer period (10ms). The WaveRT miniport derives its actual timer
// period from the buffer size + notification count requested by PortCls, but
// needs a non-zero default prior to buffer allocation.
//
#define VIRTIOSND_PERIOD_FRAMES 480
#define VIRTIOSND_PERIOD_BYTES (VIRTIOSND_PERIOD_FRAMES * VIRTIOSND_BLOCK_ALIGN)
#define VIRTIOSND_CAPTURE_PERIOD_BYTES (VIRTIOSND_PERIOD_FRAMES * VIRTIOSND_CAPTURE_BLOCK_ALIGN)

/*
 * The Aero contract defines four virtqueues (control/event/tx/rx).
 *
 * The virtio-snd WDM driver brings up all four queues. Protocol engines are
 * implemented for:
 *  - controlq: control plane (stream 0 playback + stream 1 capture)
 *  - txq: playback streaming (stream 0)
 *  - rxq: capture streaming (stream 1)
 *
 * PortCls/WaveRT miniports are expected to call into these engines; endpoint
 * plumbing lives elsewhere.
 */
#define VIRTIOSND_QUEUE_CONTROL VIRTIO_SND_QUEUE_CONTROL
#define VIRTIOSND_QUEUE_EVENT VIRTIO_SND_QUEUE_EVENT
#define VIRTIOSND_QUEUE_TX VIRTIO_SND_QUEUE_TX
#define VIRTIOSND_QUEUE_RX VIRTIO_SND_QUEUE_RX
#define VIRTIOSND_QUEUE_COUNT 4u
typedef struct _VIRTIOSND_DEVICE_EXTENSION {
    ULONG Signature;

    /*
     * WDM device objects.
     *
     * When running as a PortCls adapter, Self is the PortCls-created FDO and
     * Pdo is the PCI PDO. LowerDeviceObject is kept for virtio-pci transport
     * helper code that issues IRPs (e.g. QUERY_INTERFACE for PCI config access).
     *
     * In a typical PCI stack, LowerDeviceObject is the PDO itself.
     */
    PDEVICE_OBJECT Self;
    PDEVICE_OBJECT Pdo;
    PDEVICE_OBJECT LowerDeviceObject;

    IO_REMOVE_LOCK RemoveLock;

    /* virtio-pci modern transport (PCI capability discovery + MMIO BAR0). */
    VIRTIO_PCI_MODERN_TRANSPORT Transport;
    VIRTIO_PCI_MODERN_OS_INTERFACE TransportOs;
    PCI_BUS_INTERFACE_STANDARD PciInterface;
    BOOLEAN PciInterfaceAcquired;
    UCHAR PciCfgSpace[256];
    UINT64 NegotiatedFeatures;

    /*
     * Split virtqueue rings + queue abstractions.
     *
     * QueueSplit[] owns the DMA memory and split-ring state.
     * Queues[] provides a minimal Submit/PopUsed/Kick API used by higher-level
     * virtio-snd protocol code.
     */
    VIRTIOSND_QUEUE_SPLIT QueueSplit[VIRTIOSND_QUEUE_COUNT];
    VIRTIOSND_QUEUE Queues[VIRTIOSND_QUEUE_COUNT];

    /* Protocol engines (controlq + txq + rxq) */
    VIRTIOSND_CONTROL Control;
    VIRTIOSND_TX_ENGINE Tx;
    volatile LONG TxEngineInitialized;
    VIRTIOSND_RX_ENGINE Rx;
    volatile LONG RxEngineInitialized;

    /*
     * Interrupt plumbing.
     *
     * - Prefer message-signaled interrupts (MSI/MSI-X) when provided by PnP/INF.
     * - Fall back to legacy INTx (contract v1 default).
     *
     * When MSI-X is active, the driver programs virtio-pci MSI-X vectors using
     * the OS message numbers:
     *   - If MessageCount >= 1 + VIRTIOSND_QUEUE_COUNT:
     *       vector 0: config
     *       vector 1..4: queues 0..3 (control/event/tx/rx)
     *   - Otherwise: all on vector 0 (config + all queues)
     */

    /* Legacy INTx plumbing (shared helper in virtio_pci_intx_wdm.c). */
    VIRTIO_INTX Intx;
    CM_PARTIAL_RESOURCE_DESCRIPTOR InterruptDesc;
    BOOLEAN InterruptDescPresent;

     /*
      * Registry (per-device, under the device instance key):
      *   HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly
      *   (REG_DWORD)
      *
      * When TRUE, the driver is permitted to start even if no usable interrupt
      * resource can be discovered/connected (neither MSI/MSI-X nor legacy INTx).
      * In that case, higher layers are expected to rely on polling used rings for
      * completion delivery.
      *
      * Default: 0 / FALSE (seeded by the INF; normal interrupt-driven mode).
      */
    BOOLEAN AllowPollingOnly;

    /* Message-signaled (MSI/MSI-X) plumbing. */
    CM_PARTIAL_RESOURCE_DESCRIPTOR MessageInterruptDesc;
    BOOLEAN MessageInterruptDescPresent;
    BOOLEAN MessageInterruptsConnected;
    BOOLEAN MessageInterruptsActive; /* TRUE when using MSI/MSI-X instead of INTx. */

    /* IoConnectInterruptEx(CONNECT_MESSAGE_BASED) outputs. */
    PIO_INTERRUPT_MESSAGE_INFO MessageInterruptInfo;
    PVOID MessageInterruptConnectionContext;
    ULONG MessageInterruptCount;

    /* MSI/MSI-X DPC coalescing (similar semantics to VIRTIO_INTX::DpcInFlight). */
    KDPC MessageDpc;
    volatile LONG MessageDpcInFlight;
    volatile LONG MessagePendingMask; /* bitmask of pending MessageID values */

    /* Diagnostic counters for MSI/MSI-X (incremented via interlocked ops). */
    volatile LONG MessageIsrCount;
    volatile LONG MessageDpcCount;

    /* Device vector routing when MessageInterruptsActive==TRUE. */
    BOOLEAN MsixAllOnVector0;
    USHORT MsixConfigVector;
    USHORT MsixQueueVectors[VIRTIOSND_QUEUE_COUNT];

    /* Per-queue drain count (incremented in interrupt DPC paths). */
    volatile LONG QueueDrainCount[VIRTIOSND_QUEUE_COUNT];

    /* Optional diagnostic device object (\\.\aero_virtio_snd_diag). */
    PDEVICE_OBJECT DiagDeviceObject;

    VIRTIOSND_DMA_CONTEXT DmaCtx;

    /*
     * Cached PCM capabilities (from VIRTIO_SND_R_PCM_INFO).
     *
     * The Aero contract v1 requires S16/48kHz for both streams, but devices may
     * advertise additional formats/rates. These fields allow higher layers
     * (WaveRT pin factories + control SET_PARAMS) to remain consistent with what
     * the device actually supports.
     *
     * - PcmInfo[] stores the raw device-reported bitmasks/ranges.
     * - PcmSupportedFormats/Rates are filtered to the subset supported by this
     *   Windows 7 driver (see VIRTIOSND_PCM_DRIVER_SUPPORTED_* in
     *   virtiosnd_control_proto.h).
     * - PcmSelectedFormat/Rate track the currently-selected format/rate for each
     *   stream (defaults to S16/48kHz).
     */
    VIRTIO_SND_PCM_INFO PcmInfo[2];
    ULONGLONG PcmSupportedFormats[2];
    ULONGLONG PcmSupportedRates[2];
    UCHAR PcmSelectedFormat[2];
    UCHAR PcmSelectedRate[2];

    /* Minimal eventq RX buffer pool (see VIRTIOSND_EVENTQ_*). */
    VIRTIOSND_DMA_BUFFER EventqBufferPool;
    ULONG EventqBufferCount;
    VIRTIOSND_EVENTQ_STATS EventqStats;

    /*
     * Optional eventq callback hook (WaveRT).
     *
     * Contract v1 drivers must not depend on eventq, but future device models
     * may emit virtio-snd spec events (PCM period-elapsed / XRUN). The INTx DPC
     * parses eventq buffers and dispatches events via this callback.
     *
     * IRQL: callback is invoked at <= DISPATCH_LEVEL.
     */
    KSPIN_LOCK EventqLock;
    EVT_VIRTIOSND_EVENTQ_EVENT* EventqCallback;
    void* EventqCallbackContext;
    volatile LONG EventqCallbackInFlight;

    /*
     * Optional WaveRT notification events keyed by virtio-snd stream_id.
     *
     * The driver keeps timer-based pacing for contract v1 compatibility. If a
     * future device model emits PCM_PERIOD_ELAPSED events, the INTx DPC can use
     * them as an additional (best-effort) notification source by signaling the
     * corresponding event object.
     */
    PKEVENT EventqStreamNotify[VIRTIOSND_EVENTQ_MAX_NOTIFY_STREAMS];

    /*
     * PERIOD_ELAPSED diagnostic bookkeeping.
     *
     * Sequence counters are incremented once per PERIOD_ELAPSED event.
     * The timestamp is in 100ns units (KeQueryInterruptTime).
     */
    volatile LONG PcmPeriodSeq[VIRTIOSND_EVENTQ_MAX_NOTIFY_STREAMS];
    volatile LONGLONG PcmLastPeriodEventTime100ns[VIRTIOSND_EVENTQ_MAX_NOTIFY_STREAMS];

    /*
     * Best-effort WaveRT XRUN recovery work item (coalesced).
     *
     * XRUN events are delivered at DISPATCH_LEVEL; WaveRT recovery may require
     * PASSIVE_LEVEL control-plane operations (PCM_START). To avoid allocating or
     * queueing unbounded work items when events are spammed, we coalesce pending
     * XRUN notifications into a bitmask and process them on a single work item.
     *
     * Pending mask bit 0: stream 0 (playback), bit 1: stream 1 (capture).
     */
    WORK_QUEUE_ITEM PcmXrunWorkItem;
    volatile LONG PcmXrunWorkQueued;
    volatile LONG PcmXrunPendingMask;

    /* Jack state reflected through the PortCls topology miniport. */
    VIRTIOSND_JACK_STATE JackState;
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
 * Emergency best-effort reset used by streaming teardown paths when an in-flight
 * request never completes (e.g. device reset/misbehavior timing).
 *
 * This stops further device DMA/completions but intentionally does *not* tear
 * down the DMA context so higher layers (WaveRT cyclic buffer/MDL owners) can
 * still free their common buffers safely.
 *
 * IRQL: PASSIVE_LEVEL only.
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtIoSndHwResetDeviceForTeardown(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/*
 * Poll all relevant virtqueues for used entries and deliver completions.
 *
 * This is intended for bring-up/debug environments where no usable interrupt
 * mechanism is available (neither MSI/MSI-X nor INTx) and the driver must
 * operate in a polling-only configuration.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtIoSndHwPollAllUsed(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

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
    _In_ ULONG FrameBytes,
    _In_ ULONG MaxPeriodBytes,
    _In_ ULONG BufferCount,
    _In_ BOOLEAN SuppressInterrupts);

/*
 * Initialize the TX engine with an explicit PCM frame size (Channels * BytesPerSample).
 *
 * Contract v1 callers should continue using VirtIoSndInitTxEngine(), which
 * defaults to 4 bytes per frame (stereo S16_LE).
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtIoSndInitTxEngineEx(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_ ULONG FrameBytes,
    _In_ ULONG MaxPeriodBytes,
    _In_ ULONG BufferCount,
    _In_ BOOLEAN SuppressInterrupts);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID VirtIoSndUninitTxEngine(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/*
 * Initialize the RX (capture) engine.
 *
 * The caller should configure stream 1 via the control engine (SET_PARAMS1 /
 * PREPARE1 / START1) and provide a completion callback via
 * VirtIoSndHwSetRxCompletionCallback before submitting buffers.
 *
 * IRQL: PASSIVE_LEVEL only.
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtIoSndInitRxEngine(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx, _In_ ULONG FrameBytes, _In_ ULONG RequestCount);

/*
 * Initialize the RX engine with an explicit PCM frame size (Channels * BytesPerSample).
 *
 * Contract v1 callers should continue using VirtIoSndInitRxEngine(), which
 * defaults to 2 bytes per frame (mono S16_LE).
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtIoSndInitRxEngineEx(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_ ULONG FrameBytes,
    _In_ ULONG RequestCount);

/*
 * Tear down the RX (capture) engine.
 *
 * IRQL: PASSIVE_LEVEL only.
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID VirtIoSndUninitRxEngine(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/*
 * Set the RX completion callback invoked from the INTx DPC.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtIoSndHwSetRxCompletionCallback(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_opt_ EVT_VIRTIOSND_RX_COMPLETION* Callback,
    _In_opt_ void* Context);

/*
 * Submit an RX capture buffer as a list of DMA segments.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
_Must_inspect_result_ NTSTATUS VirtIoSndHwSubmitRxSg(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_reads_(SegmentCount) const VIRTIOSND_RX_SEGMENT* Segments,
    _In_ USHORT SegmentCount,
    _In_opt_ void* Cookie);

/*
 * Drain used completions from rxq (polling use cases).
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
ULONG VirtIoSndHwDrainRxCompletions(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_opt_ EVT_VIRTIOSND_RX_COMPLETION* Callback,
    _In_opt_ void* Context);

/*
 * Set the eventq callback invoked from the INTx DPC for parsed virtio-snd events.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtIoSndHwSetEventCallback(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_opt_ EVT_VIRTIOSND_EVENTQ_EVENT* Callback,
    _In_opt_ void* Context);

/*
 * Associate a WaveRT notification event object with a virtio-snd stream ID.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtIoSndEventqSetStreamNotificationEvent(
    _Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _In_ ULONG StreamId,
    _In_opt_ PKEVENT NotificationEvent);

/*
 * Best-effort signal of the registered WaveRT notification event for a stream.
 *
 * Returns TRUE if an event was present and signaled.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
BOOLEAN VirtIoSndEventqSignalStreamNotificationEvent(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx, _In_ ULONG StreamId);

#ifdef __cplusplus
} /* extern "C" */
#endif
