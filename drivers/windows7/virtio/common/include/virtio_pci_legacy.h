#pragma once

#include <ntddk.h>

#include "virtio_bits.h"

/*
 * Virtio PCI legacy I/O interface (split virtqueues).
 *
 * This matches the legacy virtio-pci register layout used by QEMU's
 * transitional virtio devices when MSI-X is disabled (INTx path).
 */

// Legacy virtio-pci register offsets (I/O port BAR)
#define VIRTIO_PCI_HOST_FEATURES   0x00 // u32
#define VIRTIO_PCI_GUEST_FEATURES  0x04 // u32
#define VIRTIO_PCI_QUEUE_PFN       0x08 // u32
#define VIRTIO_PCI_QUEUE_NUM       0x0C // u16 (max queue size)
#define VIRTIO_PCI_QUEUE_SEL       0x0E // u16
#define VIRTIO_PCI_QUEUE_NOTIFY    0x10 // u16
#define VIRTIO_PCI_STATUS          0x12 // u8
#define VIRTIO_PCI_ISR             0x13 // u8 (read clears)
#define VIRTIO_PCI_CONFIG_VECTOR   0x14 // u16 (only if MSI-X enabled)
#define VIRTIO_PCI_QUEUE_VECTOR    0x16 // u16 (only if MSI-X enabled)

// Device-specific config offset depends on whether MSI-X is enabled.
#define VIRTIO_PCI_DEVICE_CFG_OFF_NO_MSIX 0x14
#define VIRTIO_PCI_DEVICE_CFG_OFF_MSIX    0x18

typedef struct _VIRTIO_PCI_DEVICE {
  PUCHAR IoBase;
  ULONG IoLength;
  BOOLEAN MsixEnabled;

  ULONG HostFeatures;
  ULONG GuestFeatures;

  ULONG DeviceConfigOffset;
} VIRTIO_PCI_DEVICE;

VOID VirtioPciInitialize(_Out_ VIRTIO_PCI_DEVICE* Device, _In_ PUCHAR IoBase, _In_ ULONG IoLength,
                         _In_ BOOLEAN MsixEnabled);

VOID VirtioPciReset(_Inout_ VIRTIO_PCI_DEVICE* Device);

_Ret_range_(0, 0xFF) UCHAR VirtioPciGetStatus(_In_ const VIRTIO_PCI_DEVICE* Device);
VOID VirtioPciSetStatus(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ UCHAR Status);
VOID VirtioPciAddStatus(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ UCHAR StatusBits);

_Ret_range_(0, 0xFFFFFFFF) ULONG VirtioPciReadHostFeatures(_Inout_ VIRTIO_PCI_DEVICE* Device);
VOID VirtioPciWriteGuestFeatures(_Inout_ VIRTIO_PCI_DEVICE* Device, _In_ ULONG GuestFeatures);

_Ret_range_(0, 0xFF) UCHAR VirtioPciReadIsr(_In_ const VIRTIO_PCI_DEVICE* Device);

VOID VirtioPciSelectQueue(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ USHORT QueueIndex);
_Ret_range_(0, 0xFFFF) USHORT VirtioPciReadQueueSize(_In_ const VIRTIO_PCI_DEVICE* Device);
VOID VirtioPciWriteQueuePfn(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG QueuePfn);
VOID VirtioPciNotifyQueue(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ USHORT QueueIndex);

_Must_inspect_result_ NTSTATUS VirtioPciReadDeviceConfig(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG Offset,
                                                         _Out_writes_bytes_(Length) VOID* Buffer, _In_ ULONG Length);

