#ifndef VIRTIO_SPEC_H_
#define VIRTIO_SPEC_H_

/*
 * Minimal Virtio 1.0+ structures/constants needed by the Win7 virtio-core
 * transport layer.
 *
 * This header intentionally avoids any driver/framework-specific dependencies
 * so it can be shared across virtio-* drivers.
 */

#include <ntddk.h>

#ifndef VIRTIO_PCI_MAX_BARS
#define VIRTIO_PCI_MAX_BARS 6
#endif

/* Virtio 1.0 feature bit indicating a modern (1.0+) device. */
#ifndef VIRTIO_F_VERSION_1
#define VIRTIO_F_VERSION_1 (1ui64 << 32)
#endif

/* Common device status bits (virtio spec "Device Status Field"). */
#define VIRTIO_STATUS_ACKNOWLEDGE        0x01
#define VIRTIO_STATUS_DRIVER             0x02
#define VIRTIO_STATUS_DRIVER_OK          0x04
#define VIRTIO_STATUS_FEATURES_OK        0x08
#define VIRTIO_STATUS_DEVICE_NEEDS_RESET 0x40
#define VIRTIO_STATUS_FAILED             0x80

/*
 * Compatibility aliases.
 *
 * Some Virtio codebases (including the task spec for this repo) use the
 * VIRTIO_CONFIG_S_* naming. Keep the canonical VIRTIO_STATUS_* names and
 * provide aliases to avoid churn.
 */
#ifndef VIRTIO_CONFIG_S_ACKNOWLEDGE
#define VIRTIO_CONFIG_S_ACKNOWLEDGE VIRTIO_STATUS_ACKNOWLEDGE
#endif
#ifndef VIRTIO_CONFIG_S_DRIVER
#define VIRTIO_CONFIG_S_DRIVER VIRTIO_STATUS_DRIVER
#endif
#ifndef VIRTIO_CONFIG_S_DRIVER_OK
#define VIRTIO_CONFIG_S_DRIVER_OK VIRTIO_STATUS_DRIVER_OK
#endif
#ifndef VIRTIO_CONFIG_S_FEATURES_OK
#define VIRTIO_CONFIG_S_FEATURES_OK VIRTIO_STATUS_FEATURES_OK
#endif
#ifndef VIRTIO_CONFIG_S_DEVICE_NEEDS_RESET
#define VIRTIO_CONFIG_S_DEVICE_NEEDS_RESET VIRTIO_STATUS_DEVICE_NEEDS_RESET
#endif
#ifndef VIRTIO_CONFIG_S_FAILED
#define VIRTIO_CONFIG_S_FAILED VIRTIO_STATUS_FAILED
#endif

#pragma pack(push, 1)

/*
 * Virtio PCI "common configuration" structure (virtio spec:
 * "Virtio Over PCI Bus -> Common configuration structure").
 *
 * Note: The spec defines 64-bit queue addresses, but using 32-bit lo/hi fields
 * avoids unaligned 64-bit MMIO accesses on Windows.
 */
typedef struct virtio_pci_common_cfg {
    ULONG device_feature_select; /* read-write */
    ULONG device_feature;        /* read-only  */
    ULONG driver_feature_select; /* read-write */
    ULONG driver_feature;        /* read-write */
    USHORT msix_config;          /* read-write */
    USHORT num_queues;           /* read-only  */
    UCHAR device_status;         /* read-write */
    UCHAR config_generation;     /* read-only  */

    USHORT queue_select;      /* read-write */
    USHORT queue_size;        /* read-only  */
    USHORT queue_msix_vector; /* read-write */
    USHORT queue_enable;      /* read-write */
    USHORT queue_notify_off;  /* read-only  */
    ULONG queue_desc_lo;      /* read-write */
    ULONG queue_desc_hi;      /* read-write */
    ULONG queue_avail_lo;     /* read-write */
    ULONG queue_avail_hi;     /* read-write */
    ULONG queue_used_lo;      /* read-write */
    ULONG queue_used_hi;      /* read-write */
} virtio_pci_common_cfg, *Pvirtio_pci_common_cfg;

#pragma pack(pop)

/*
 * CommonCfg offsets are defined by the virtio spec. Assert the layout so any
 * accidental padding or stray fields are caught at compile time.
 */
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_feature_select) == 0x00);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_feature) == 0x04);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, driver_feature_select) == 0x08);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, driver_feature) == 0x0C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, msix_config) == 0x10);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, num_queues) == 0x12);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_status) == 0x14);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, config_generation) == 0x15);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_select) == 0x16);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_size) == 0x18);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_msix_vector) == 0x1A);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_enable) == 0x1C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_notify_off) == 0x1E);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc_lo) == 0x20);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc_hi) == 0x24);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail_lo) == 0x28);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail_hi) == 0x2C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used_lo) == 0x30);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used_hi) == 0x34);
C_ASSERT(sizeof(virtio_pci_common_cfg) == 0x38);

#endif /* VIRTIO_SPEC_H_ */
