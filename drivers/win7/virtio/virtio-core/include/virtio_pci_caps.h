#ifndef VIRTIO_PCI_CAPS_H_
#define VIRTIO_PCI_CAPS_H_

/*
 * Virtio PCI vendor-specific capability definitions and discovery output.
 *
 * These structures follow the Virtio 1.0+ specification for "PCI Device
 * Discovery" / "Virtio PCI Capability".
 */

#include <ntddk.h>

#include "virtio_spec.h"

/*
 * Standard PCI capability ID for vendor-specific capabilities.
 * (Do not rely on WDK's PCI_CAPABILITY_ID_* naming, keep this local.)
 */
#define VIRTIO_PCI_CAP_ID_VENDOR_SPECIFIC 0x09

/* Virtio vendor capability types (virtio spec). */
#define VIRTIO_PCI_CAP_COMMON_CFG 1
#define VIRTIO_PCI_CAP_NOTIFY_CFG 2
#define VIRTIO_PCI_CAP_ISR_CFG    3
#define VIRTIO_PCI_CAP_DEVICE_CFG 4
#define VIRTIO_PCI_CAP_PCI_CFG    5

#define VIRTIO_PCI_MAX_CAPS 32

#pragma pack(push, 1)

struct virtio_pci_cap {
    UCHAR CapVndr; /* VIRTIO_PCI_CAP_ID_VENDOR_SPECIFIC */
    UCHAR CapNext;
    UCHAR CapLen;
    UCHAR CfgType;
    UCHAR Bar;
    UCHAR Id;
    UCHAR Padding[2];
    ULONG Offset;
    ULONG Length;
};

struct virtio_pci_notify_cap {
    struct virtio_pci_cap Cap;
    ULONG NotifyOffMultiplier;
};

#pragma pack(pop)

typedef struct _VIRTIO_PCI_CAP_INFO {
    BOOLEAN Present;
    UCHAR CfgType;
    UCHAR Bar;
    UCHAR Id;
    UCHAR CapLen;
    ULONG CapOffset; /* PCI config space offset of the capability header */
    ULONG Offset;    /* Offset within BAR */
    ULONG Length;    /* Length within BAR */
} VIRTIO_PCI_CAP_INFO, *PVIRTIO_PCI_CAP_INFO;

typedef struct _VIRTIO_PCI_CAPS {
    VIRTIO_PCI_CAP_INFO CommonCfg;
    VIRTIO_PCI_CAP_INFO NotifyCfg;
    VIRTIO_PCI_CAP_INFO IsrCfg;
    VIRTIO_PCI_CAP_INFO DeviceCfg;

    ULONG NotifyOffMultiplier;

    /* All virtio_pci_cap entries discovered while walking the cap list. */
    VIRTIO_PCI_CAP_INFO All[VIRTIO_PCI_MAX_CAPS];
    ULONG AllCount;
} VIRTIO_PCI_CAPS, *PVIRTIO_PCI_CAPS;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciCapsDiscover(_In_ PPCI_BUS_INTERFACE_STANDARD PciInterface,
                      _In_ const ULONGLONG BarBases[VIRTIO_PCI_MAX_BARS],
                      _Out_ PVIRTIO_PCI_CAPS Caps);

#endif /* VIRTIO_PCI_CAPS_H_ */
