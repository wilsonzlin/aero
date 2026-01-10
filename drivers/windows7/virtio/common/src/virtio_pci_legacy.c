#include "../include/virtio_pci_legacy.h"

static __forceinline UCHAR VirtioPciRead8(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG Offset) {
  return READ_PORT_UCHAR(Device->IoBase + Offset);
}

static __forceinline USHORT VirtioPciRead16(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG Offset) {
  return READ_PORT_USHORT((PUSHORT)(Device->IoBase + Offset));
}

static __forceinline ULONG VirtioPciRead32(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG Offset) {
  return READ_PORT_ULONG((PULONG)(Device->IoBase + Offset));
}

static __forceinline VOID VirtioPciWrite8(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG Offset, _In_ UCHAR Value) {
  WRITE_PORT_UCHAR(Device->IoBase + Offset, Value);
}

static __forceinline VOID VirtioPciWrite16(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG Offset, _In_ USHORT Value) {
  WRITE_PORT_USHORT((PUSHORT)(Device->IoBase + Offset), Value);
}

static __forceinline VOID VirtioPciWrite32(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG Offset, _In_ ULONG Value) {
  WRITE_PORT_ULONG((PULONG)(Device->IoBase + Offset), Value);
}

VOID VirtioPciInitialize(_Out_ VIRTIO_PCI_DEVICE* Device, _In_ PUCHAR IoBase, _In_ ULONG IoLength,
                         _In_ BOOLEAN MsixEnabled) {
  RtlZeroMemory(Device, sizeof(*Device));

  Device->IoBase = IoBase;
  Device->IoLength = IoLength;
  Device->MsixEnabled = MsixEnabled ? TRUE : FALSE;
  Device->DeviceConfigOffset = MsixEnabled ? VIRTIO_PCI_DEVICE_CFG_OFF_MSIX : VIRTIO_PCI_DEVICE_CFG_OFF_NO_MSIX;
}

VOID VirtioPciReset(_Inout_ VIRTIO_PCI_DEVICE* Device) {
  UNREFERENCED_PARAMETER(Device);
  VirtioPciWrite8(Device, VIRTIO_PCI_STATUS, 0);
  KeMemoryBarrier();
}

UCHAR VirtioPciGetStatus(_In_ const VIRTIO_PCI_DEVICE* Device) { return VirtioPciRead8(Device, VIRTIO_PCI_STATUS); }

VOID VirtioPciSetStatus(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ UCHAR Status) {
  VirtioPciWrite8(Device, VIRTIO_PCI_STATUS, Status);
  KeMemoryBarrier();
}

VOID VirtioPciAddStatus(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ UCHAR StatusBits) {
  UCHAR Status = VirtioPciGetStatus(Device);
  Status |= StatusBits;
  VirtioPciSetStatus(Device, Status);
}

ULONG VirtioPciReadHostFeatures(_Inout_ VIRTIO_PCI_DEVICE* Device) {
  Device->HostFeatures = VirtioPciRead32(Device, VIRTIO_PCI_HOST_FEATURES);
  return Device->HostFeatures;
}

VOID VirtioPciWriteGuestFeatures(_Inout_ VIRTIO_PCI_DEVICE* Device, _In_ ULONG GuestFeatures) {
  Device->GuestFeatures = GuestFeatures;
  VirtioPciWrite32(Device, VIRTIO_PCI_GUEST_FEATURES, GuestFeatures);
  KeMemoryBarrier();
}

UCHAR VirtioPciReadIsr(_In_ const VIRTIO_PCI_DEVICE* Device) { return VirtioPciRead8(Device, VIRTIO_PCI_ISR); }

VOID VirtioPciSelectQueue(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ USHORT QueueIndex) {
  VirtioPciWrite16(Device, VIRTIO_PCI_QUEUE_SEL, QueueIndex);
  KeMemoryBarrier();
}

USHORT VirtioPciReadQueueSize(_In_ const VIRTIO_PCI_DEVICE* Device) { return VirtioPciRead16(Device, VIRTIO_PCI_QUEUE_NUM); }

VOID VirtioPciWriteQueuePfn(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG QueuePfn) {
  VirtioPciWrite32(Device, VIRTIO_PCI_QUEUE_PFN, QueuePfn);
  KeMemoryBarrier();
}

VOID VirtioPciNotifyQueue(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ USHORT QueueIndex) {
  VirtioPciWrite16(Device, VIRTIO_PCI_QUEUE_NOTIFY, QueueIndex);
  KeMemoryBarrier();
}

NTSTATUS VirtioPciReadDeviceConfig(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ ULONG Offset, _Out_writes_bytes_(Length) VOID* Buffer,
                                   _In_ ULONG Length) {
  PUCHAR Out = (PUCHAR)Buffer;
  ULONG I;

  if (Offset + Length < Offset) {
    return STATUS_INVALID_PARAMETER;
  }

  if (Device->DeviceConfigOffset + Offset + Length > Device->IoLength) {
    // The caller likely passed a truncated resource length.
    return STATUS_BUFFER_TOO_SMALL;
  }

  for (I = 0; I < Length; I++) {
    Out[I] = VirtioPciRead8(Device, Device->DeviceConfigOffset + Offset + I);
  }

  return STATUS_SUCCESS;
}

