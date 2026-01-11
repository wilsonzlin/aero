/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Aero virtio-pci "modern" transport (contract v1).
 *
 * Scope (see docs/windows7-virtio-driver-contract.md):
 *   - virtio-pci modern (virtio 1.0+) only
 *   - Fixed BAR0 MMIO layout:
 *       common=0x0000, notify=0x1000, isr=0x2000, device=0x3000
 *       BAR size >= 0x4000
 *   - notify_off_multiplier fixed to 4
 *   - split virtqueues only (no packed ring)
 *   - INTx ISR read-to-ack semantics
 *
 * This library intentionally avoids any KMDF / WDF types so it can be used from
 * StorPort miniports, NDIS miniports, and WDM drivers.
 */

#ifndef AERO_VIRTIO_PCI_MODERN_H_
#define AERO_VIRTIO_PCI_MODERN_H_

#include <stddef.h>
#include <stdint.h>

/*
 * Kernel-mode build detection.
 *
 * WDK toolchains commonly define _KERNEL_MODE, but some driver build setups only
 * expose the WDK header guards (_NTDDK_, _NTIFS_, _WDMDDK_). Treat those as
 * kernel-mode as well so consumers (StorPort/NDIS/WDM) can include this header
 * without having to define _KERNEL_MODE explicitly.
 */
#if defined(_WIN32) && (defined(_KERNEL_MODE) || defined(_NTDDK_) || defined(_NTIFS_) || defined(_WDMDDK_))
#define AERO_VIRTIO_PCI_MODERN_KERNEL_MODE 1
#else
#define AERO_VIRTIO_PCI_MODERN_KERNEL_MODE 0
#endif

#if AERO_VIRTIO_PCI_MODERN_KERNEL_MODE
#include <ntddk.h>
#endif

/*
 * Host-side unit tests build this header without WDK headers.
 * Provide a minimal set of WDK-compatible types/constants for that case.
 */
#if !AERO_VIRTIO_PCI_MODERN_KERNEL_MODE

typedef uint8_t UCHAR;
typedef uint16_t USHORT;
typedef uint32_t ULONG;
typedef uint64_t ULONGLONG;
typedef uint8_t BOOLEAN;
typedef uint8_t KIRQL;
typedef ULONG KSPIN_LOCK;
typedef int32_t NTSTATUS;

#ifndef TRUE
#define TRUE 1
#endif
#ifndef FALSE
#define FALSE 0
#endif

#ifndef STATUS_SUCCESS
#define STATUS_SUCCESS ((NTSTATUS)0)
#endif
#ifndef STATUS_INVALID_PARAMETER
#define STATUS_INVALID_PARAMETER ((NTSTATUS)-1)
#endif
#ifndef STATUS_NOT_SUPPORTED
#define STATUS_NOT_SUPPORTED ((NTSTATUS)-2)
#endif
#ifndef STATUS_NOT_FOUND
#define STATUS_NOT_FOUND ((NTSTATUS)-3)
#endif
#ifndef STATUS_IO_TIMEOUT
#define STATUS_IO_TIMEOUT ((NTSTATUS)-4)
#endif
#ifndef STATUS_IO_DEVICE_ERROR
#define STATUS_IO_DEVICE_ERROR ((NTSTATUS)-5)
#endif

#ifndef FIELD_OFFSET
#define FIELD_OFFSET(type, field) offsetof(type, field)
#endif

#ifndef C_ASSERT
#define AERO_VIRTIO_C_ASSERT_GLUE(a, b) a##b
#define AERO_VIRTIO_C_ASSERT_XGLUE(a, b) AERO_VIRTIO_C_ASSERT_GLUE(a, b)
#define C_ASSERT(e) typedef char AERO_VIRTIO_C_ASSERT_XGLUE(_aero_virtio_c_assert_, __LINE__)[(e) ? 1 : -1]
#endif

#endif /* !AERO_VIRTIO_PCI_MODERN_KERNEL_MODE */

/* -------------------------------------------------------------------------- */
/* Fixed contract v1 MMIO layout                                              */
/* -------------------------------------------------------------------------- */

#define AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE 0x4000u

#define AERO_VIRTIO_PCI_MODERN_COMMON_CFG_OFFSET 0x0000u
#define AERO_VIRTIO_PCI_MODERN_COMMON_CFG_SIZE 0x0100u

#define AERO_VIRTIO_PCI_MODERN_NOTIFY_OFFSET 0x1000u
#define AERO_VIRTIO_PCI_MODERN_NOTIFY_SIZE 0x0100u

#define AERO_VIRTIO_PCI_MODERN_ISR_OFFSET 0x2000u
#define AERO_VIRTIO_PCI_MODERN_ISR_SIZE 0x0020u

#define AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_OFFSET 0x3000u
#define AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_SIZE 0x0100u

#define AERO_VIRTIO_PCI_MODERN_NOTIFY_OFF_MULTIPLIER 4u

/* -------------------------------------------------------------------------- */
/* Virtio spec bits (minimal subset)                                          */
/* -------------------------------------------------------------------------- */

#ifndef VIRTIO_F_VERSION_1
#define VIRTIO_F_VERSION_1 (1ULL << 32)
#endif

/* Common virtio device status bits. */
#define VIRTIO_STATUS_ACKNOWLEDGE 0x01u
#define VIRTIO_STATUS_DRIVER 0x02u
#define VIRTIO_STATUS_DRIVER_OK 0x04u
#define VIRTIO_STATUS_FEATURES_OK 0x08u
#define VIRTIO_STATUS_DEVICE_NEEDS_RESET 0x40u
#define VIRTIO_STATUS_FAILED 0x80u

/* ISR status bits (read-to-ack). */
#define VIRTIO_PCI_ISR_QUEUE 0x01u
#define VIRTIO_PCI_ISR_CONFIG 0x02u

/* -------------------------------------------------------------------------- */
/* virtio_pci_common_cfg (contract v1 exact layout)                           */
/* -------------------------------------------------------------------------- */

#pragma pack(push, 1)

typedef struct virtio_pci_common_cfg {
    ULONG device_feature_select; /* 0x00 - R/W */
    ULONG device_feature;        /* 0x04 - R   */
    ULONG driver_feature_select; /* 0x08 - R/W */
    ULONG driver_feature;        /* 0x0C - R/W */
    USHORT msix_config;          /* 0x10 - R/W */
    USHORT num_queues;           /* 0x12 - R   */
    UCHAR device_status;         /* 0x14 - R/W */
    UCHAR config_generation;     /* 0x15 - R   */

    USHORT queue_select;      /* 0x16 - R/W */
    USHORT queue_size;        /* 0x18 - R   */
    USHORT queue_msix_vector; /* 0x1A - R/W */
    USHORT queue_enable;      /* 0x1C - R/W */
    USHORT queue_notify_off;  /* 0x1E - R   */

    /*
     * The spec defines these as 64-bit fields, but Windows 7 drivers should
     * program them using 32-bit MMIO accesses.
     */
    union {
        ULONGLONG queue_desc; /* 0x20 - R/W */
        struct {
            ULONG queue_desc_lo;
            ULONG queue_desc_hi;
        };
    };

    union {
        ULONGLONG queue_avail; /* 0x28 - R/W */
        struct {
            ULONG queue_avail_lo;
            ULONG queue_avail_hi;
        };
    };

    union {
        ULONGLONG queue_used; /* 0x30 - R/W */
        struct {
            ULONG queue_used_lo;
            ULONG queue_used_hi;
        };
    };
} virtio_pci_common_cfg;

#pragma pack(pop)

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
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc) == 0x20);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc_lo) == 0x20);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc_hi) == 0x24);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail) == 0x28);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail_lo) == 0x28);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail_hi) == 0x2C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used) == 0x30);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used_lo) == 0x30);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used_hi) == 0x34);
C_ASSERT(sizeof(virtio_pci_common_cfg) == 0x38);

/* -------------------------------------------------------------------------- */
/* Device wrapper                                                             */
/* -------------------------------------------------------------------------- */

typedef struct _AERO_VIRTIO_PCI_MODERN_DEVICE {
    volatile virtio_pci_common_cfg *CommonCfg;
    volatile UCHAR *NotifyBase;
    volatile UCHAR *IsrStatus;
    volatile UCHAR *DeviceCfg;
    ULONG NotifyOffMultiplier; /* fixed to 4 for contract v1 */

    /* Serializes accesses that use common_cfg selector registers. */
    KSPIN_LOCK CommonCfgLock;
} AERO_VIRTIO_PCI_MODERN_DEVICE;

/* -------------------------------------------------------------------------- */
/* Public API                                                                 */
/* -------------------------------------------------------------------------- */

NTSTATUS AeroVirtioPciModernInitFromBar0(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, volatile void *Bar0Va, ULONG Bar0Len);

KIRQL AeroVirtioCommonCfgLock(AERO_VIRTIO_PCI_MODERN_DEVICE *Device);
void AeroVirtioCommonCfgUnlock(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, KIRQL OldIrql);

void AeroVirtioResetDevice(AERO_VIRTIO_PCI_MODERN_DEVICE *Device);
void AeroVirtioAddStatus(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, UCHAR StatusBits);
UCHAR AeroVirtioGetStatus(AERO_VIRTIO_PCI_MODERN_DEVICE *Device);
void AeroVirtioSetStatus(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, UCHAR Status);
void AeroVirtioFailDevice(AERO_VIRTIO_PCI_MODERN_DEVICE *Device);

ULONGLONG AeroVirtioReadDeviceFeatures(AERO_VIRTIO_PCI_MODERN_DEVICE *Device);
void AeroVirtioWriteDriverFeatures(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, ULONGLONG Features);

NTSTATUS AeroVirtioNegotiateFeatures(AERO_VIRTIO_PCI_MODERN_DEVICE *Device,
                                     ULONGLONG Required,
                                     ULONGLONG Wanted,
                                     ULONGLONG *NegotiatedOut);

USHORT AeroVirtioGetNumQueues(AERO_VIRTIO_PCI_MODERN_DEVICE *Device);

NTSTATUS AeroVirtioQueryQueue(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, USHORT QueueIndex, USHORT *QueueSizeOut, USHORT *QueueNotifyOffOut);
NTSTATUS AeroVirtioSetupQueue(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, USHORT QueueIndex, ULONGLONG DescPa, ULONGLONG AvailPa, ULONGLONG UsedPa);

void AeroVirtioNotifyQueue(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, USHORT QueueIndex, USHORT QueueNotifyOff);

UCHAR AeroVirtioReadIsr(AERO_VIRTIO_PCI_MODERN_DEVICE *Device);

NTSTATUS AeroVirtioReadDeviceConfig(AERO_VIRTIO_PCI_MODERN_DEVICE *Device, ULONG Offset, void *Buffer, ULONG Length);

#endif /* AERO_VIRTIO_PCI_MODERN_H_ */
