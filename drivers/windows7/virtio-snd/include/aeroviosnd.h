/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "../../virtio/common/include/virtio_bits.h"
#include "../../virtio/common/include/virtio_pci_legacy.h"
#include "../../virtio/common/include/virtio_queue.h"

/*
 * PortCls/WaveRT miniport implementation for Aero virtio-snd (Windows 7).
 *
 * This header intentionally does not share structs with the WDM/modern transport
 * bring-up code in `include/virtiosnd.h`. The PortCls driver uses the existing
 * in-tree legacy virtio-pci I/O-port transport under `drivers/windows7/virtio/common`.
 */

#ifndef VIRTIOSND_POOL_TAG
#define VIRTIOSND_POOL_TAG 'dnSV' // 'VSnd' (endianness depends on debugger display)
#endif

//
// PortCls subdevice names (must match the driver's PcRegisterSubdevice names).
//
#ifndef VIRTIOSND_SUBDEVICE_WAVE
#define VIRTIOSND_SUBDEVICE_WAVE L"Wave"
#endif
#ifndef VIRTIOSND_SUBDEVICE_TOPOLOGY
#define VIRTIOSND_SUBDEVICE_TOPOLOGY L"Topology"
#endif

//
// Miniport pin IDs.
//
#ifndef VIRTIOSND_WAVE_PIN_RENDER
#define VIRTIOSND_WAVE_PIN_RENDER 0
#endif
#ifndef VIRTIOSND_WAVE_PIN_BRIDGE
#define VIRTIOSND_WAVE_PIN_BRIDGE 1
#endif

#ifndef VIRTIOSND_TOPO_PIN_BRIDGE
#define VIRTIOSND_TOPO_PIN_BRIDGE 0
#endif
#ifndef VIRTIOSND_TOPO_PIN_SPEAKER
#define VIRTIOSND_TOPO_PIN_SPEAKER 1
#endif

#define VIRTIOSND_STREAM_ID_PLAYBACK 0u

#ifndef VIRTIOSND_QUEUE_CONTROL
#define VIRTIOSND_QUEUE_CONTROL 0u
#endif
#ifndef VIRTIOSND_QUEUE_EVENT
#define VIRTIOSND_QUEUE_EVENT 1u
#endif
#ifndef VIRTIOSND_QUEUE_TX
#define VIRTIOSND_QUEUE_TX 2u
#endif
#ifndef VIRTIOSND_QUEUE_RX
#define VIRTIOSND_QUEUE_RX 3u
#endif

// Fixed-format contract for the in-tree virtio-snd device model.
#ifndef VIRTIOSND_SAMPLE_RATE
#define VIRTIOSND_SAMPLE_RATE 48000u
#endif
#ifndef VIRTIOSND_CHANNELS
#define VIRTIOSND_CHANNELS 2u
#endif
#ifndef VIRTIOSND_BITS_PER_SAMPLE
#define VIRTIOSND_BITS_PER_SAMPLE 16u
#endif
#ifndef VIRTIOSND_BYTES_PER_SAMPLE
#define VIRTIOSND_BYTES_PER_SAMPLE (VIRTIOSND_BITS_PER_SAMPLE / 8u)
#endif
#ifndef VIRTIOSND_BLOCK_ALIGN
#define VIRTIOSND_BLOCK_ALIGN (VIRTIOSND_CHANNELS * VIRTIOSND_BYTES_PER_SAMPLE)
#endif
#ifndef VIRTIOSND_AVG_BYTES_PER_SEC
#define VIRTIOSND_AVG_BYTES_PER_SEC (VIRTIOSND_SAMPLE_RATE * VIRTIOSND_BLOCK_ALIGN)
#endif

#ifndef VIRTIOSND_PERIOD_FRAMES
#define VIRTIOSND_PERIOD_FRAMES 480u // 10ms at 48kHz
#endif
#ifndef VIRTIOSND_PERIOD_BYTES
#define VIRTIOSND_PERIOD_BYTES (VIRTIOSND_PERIOD_FRAMES * VIRTIOSND_BLOCK_ALIGN)
#endif

// Default WaveRT buffer/period sizing.
#ifndef VIRTIOSND_DEFAULT_PERIOD_FRAMES
#define VIRTIOSND_DEFAULT_PERIOD_FRAMES VIRTIOSND_PERIOD_FRAMES
#endif
#ifndef VIRTIOSND_DEFAULT_PERIOD_BYTES
#define VIRTIOSND_DEFAULT_PERIOD_BYTES VIRTIOSND_PERIOD_BYTES
#endif
#ifndef VIRTIOSND_DEFAULT_BUFFER_PERIODS
#define VIRTIOSND_DEFAULT_BUFFER_PERIODS 4u
#endif
#ifndef VIRTIOSND_DEFAULT_BUFFER_BYTES
#define VIRTIOSND_DEFAULT_BUFFER_BYTES (VIRTIOSND_DEFAULT_PERIOD_BYTES * VIRTIOSND_DEFAULT_BUFFER_PERIODS)
#endif

typedef enum _VIRTIOSND_PCM_STATE {
  VirtIoSndPcmIdle = 0,
  VirtIoSndPcmParamsSet = 1,
  VirtIoSndPcmPrepared = 2,
  VirtIoSndPcmRunning = 3,
} VIRTIOSND_PCM_STATE;

typedef struct _AEROVIOSND_TX_ENTRY {
  LIST_ENTRY Link;
  PUCHAR BufferVa;
  PHYSICAL_ADDRESS BufferPa;
  ULONG PayloadBytes;
  USHORT HeadId;
} AEROVIOSND_TX_ENTRY, *PAEROVIOSND_TX_ENTRY;

typedef struct _AEROVIOSND_DEVICE_EXTENSION {
  PDEVICE_OBJECT DeviceObject;
  volatile LONG RefCount;

  // Resources
  ULONG IoPortStart;
  PUCHAR IoBase;
  ULONG IoLength;

  ULONG InterruptVector;
  KIRQL InterruptIrql;
  KAFFINITY InterruptAffinity;
  KINTERRUPT_MODE InterruptMode;

  PKINTERRUPT InterruptObject;
  KDPC InterruptDpc;

  KSPIN_LOCK Lock;

  VIRTIO_PCI_DEVICE Vdev;
  ULONG HostFeatures;
  ULONG NegotiatedFeatures;

  VIRTIO_QUEUE ControlVq;
  VIRTIO_QUEUE TxVq;

  // Control queue scratch buffer (physically contiguous).
  PUCHAR ControlBufferVa;
  PHYSICAL_ADDRESS ControlBufferPa;
  ULONG ControlBufferBytes;
  KMUTEX ControlMutex;

  // TX buffer pool (physically contiguous).
  AEROVIOSND_TX_ENTRY* TxEntries;
  ULONG TxEntryCount;
  PUCHAR TxBufferVa;
  PHYSICAL_ADDRESS TxBufferPa;
  ULONG TxBufferBytes;
  LIST_ENTRY TxFreeList;
  LIST_ENTRY TxSubmittedList;

  ULONG BufferBytes;
  ULONG PeriodBytes;
  VIRTIOSND_PCM_STATE PcmState;

  BOOLEAN Started;
} AEROVIOSND_DEVICE_EXTENSION, *PAEROVIOSND_DEVICE_EXTENSION;

#ifdef __cplusplus
extern "C" {
#endif

_Must_inspect_result_ NTSTATUS VirtIoSndHwStart(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx, _In_ PIRP StartIrp);
VOID VirtIoSndHwStop(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx);

_Must_inspect_result_ NTSTATUS VirtIoSndHwSetPcmParams(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx, _In_ ULONG BufferBytes,
                                                       _In_ ULONG PeriodBytes);
_Must_inspect_result_ NTSTATUS VirtIoSndHwStartPcm(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx);
_Must_inspect_result_ NTSTATUS VirtIoSndHwStopPcm(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx);
_Must_inspect_result_ NTSTATUS VirtIoSndHwReleasePcm(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx);

_Must_inspect_result_ NTSTATUS VirtIoSndHwSubmitTx(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx,
                                                   _In_reads_bytes_(Bytes) const VOID* Data, _In_ ULONG Bytes);

VOID VirtIoSndMiniportAddRef(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx);
VOID VirtIoSndMiniportReleaseRef(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx);

#ifdef __cplusplus
} // extern "C"
#endif
