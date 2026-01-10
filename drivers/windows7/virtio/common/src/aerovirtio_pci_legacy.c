#include <ntddk.h>
#include <storport.h>

#include "../include/aerovirtio_pci_legacy.h"

static __forceinline PUCHAR AerovirtioPciLegacyPtr(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset)
{
    return (PUCHAR)(dev->Base + offset);
}

UCHAR AerovirtioPciLegacyRead8(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset)
{
    const PUCHAR p = AerovirtioPciLegacyPtr(dev, offset);
    if (dev->AccessType == AerovirtioPciAccessPort) {
        return StorPortReadPortUchar(p);
    }
    return StorPortReadRegisterUchar(p);
}

USHORT AerovirtioPciLegacyRead16(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset)
{
    const PUSHORT p = (PUSHORT)AerovirtioPciLegacyPtr(dev, offset);
    if (dev->AccessType == AerovirtioPciAccessPort) {
        return StorPortReadPortUshort(p);
    }
    return StorPortReadRegisterUshort(p);
}

ULONG AerovirtioPciLegacyRead32(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset)
{
    const PULONG p = (PULONG)AerovirtioPciLegacyPtr(dev, offset);
    if (dev->AccessType == AerovirtioPciAccessPort) {
        return StorPortReadPortUlong(p);
    }
    return StorPortReadRegisterUlong(p);
}

VOID AerovirtioPciLegacyWrite8(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset, _In_ UCHAR val)
{
    const PUCHAR p = AerovirtioPciLegacyPtr(dev, offset);
    if (dev->AccessType == AerovirtioPciAccessPort) {
        StorPortWritePortUchar(p, val);
        return;
    }
    StorPortWriteRegisterUchar(p, val);
}

VOID AerovirtioPciLegacyWrite16(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset, _In_ USHORT val)
{
    const PUSHORT p = (PUSHORT)AerovirtioPciLegacyPtr(dev, offset);
    if (dev->AccessType == AerovirtioPciAccessPort) {
        StorPortWritePortUshort(p, val);
        return;
    }
    StorPortWriteRegisterUshort(p, val);
}

VOID AerovirtioPciLegacyWrite32(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset, _In_ ULONG val)
{
    const PULONG p = (PULONG)AerovirtioPciLegacyPtr(dev, offset);
    if (dev->AccessType == AerovirtioPciAccessPort) {
        StorPortWritePortUlong(p, val);
        return;
    }
    StorPortWriteRegisterUlong(p, val);
}

VOID AerovirtioPciLegacyReset(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev)
{
    AerovirtioPciLegacyWrite8(dev, AEROVIRTIO_PCI_LEGACY_STATUS, 0);
    KeStallExecutionProcessor(1000);
}

UCHAR AerovirtioPciLegacyGetStatus(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev)
{
    return AerovirtioPciLegacyRead8(dev, AEROVIRTIO_PCI_LEGACY_STATUS);
}

VOID AerovirtioPciLegacySetStatus(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ UCHAR status)
{
    AerovirtioPciLegacyWrite8(dev, AEROVIRTIO_PCI_LEGACY_STATUS, status);
}

ULONG AerovirtioPciLegacyReadHostFeatures(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev)
{
    return AerovirtioPciLegacyRead32(dev, AEROVIRTIO_PCI_LEGACY_HOST_FEATURES);
}

VOID AerovirtioPciLegacyWriteGuestFeatures(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG features)
{
    AerovirtioPciLegacyWrite32(dev, AEROVIRTIO_PCI_LEGACY_GUEST_FEATURES, features);
}

VOID AerovirtioPciLegacySelectQueue(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ USHORT queueIndex)
{
    AerovirtioPciLegacyWrite16(dev, AEROVIRTIO_PCI_LEGACY_QUEUE_SEL, queueIndex);
}

USHORT AerovirtioPciLegacyReadQueueSize(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev)
{
    return (USHORT)AerovirtioPciLegacyRead16(dev, AEROVIRTIO_PCI_LEGACY_QUEUE_NUM);
}

VOID AerovirtioPciLegacyWriteQueuePfn(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG queuePfn)
{
    AerovirtioPciLegacyWrite32(dev, AEROVIRTIO_PCI_LEGACY_QUEUE_PFN, queuePfn);
}

VOID AerovirtioPciLegacyNotifyQueue(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ USHORT queueIndex)
{
    AerovirtioPciLegacyWrite16(dev, AEROVIRTIO_PCI_LEGACY_QUEUE_NOTIFY, queueIndex);
}

UCHAR AerovirtioPciLegacyReadIsr(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev)
{
    return AerovirtioPciLegacyRead8(dev, AEROVIRTIO_PCI_LEGACY_ISR);
}

VOID AerovirtioPciLegacyReadDeviceConfig(
    _In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev,
    _In_ ULONG offset,
    _Out_writes_bytes_(len) PVOID buf,
    _In_ ULONG len)
{
    PUCHAR out = (PUCHAR)buf;
    for (ULONG i = 0; i < len; ++i) {
        out[i] = AerovirtioPciLegacyRead8(dev, AEROVIRTIO_PCI_LEGACY_CONFIG + offset + i);
    }
}

