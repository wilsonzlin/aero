#pragma once

/*
 * Shared diagnostics IOCTL contract for aero_virtio_net.
 *
 * This header is intentionally WDK-free so it can be included by both:
 *   - kernel-mode NDIS miniport driver (aero_virtio_net.sys)
 *   - user-mode guest selftest (aero-virtio-selftest.exe)
 *
 * Keeping the IOCTL structs/constants in one place prevents silent layout drift.
 */

#include <stdint.h>

/*
 * User-mode device path (Win32 symbolic link) for the aero_virtio_net diagnostics interface.
 *
 * Kernel-mode device name:   \\Device\\AeroVirtioNetDiag
 * Kernel-mode symlink:       \\DosDevices\\AeroVirtioNetDiag
 * User-mode CreateFile path: \\\\.\\AeroVirtioNetDiag
 */
#define AEROVNET_DIAG_DEVICE_PATH L"\\\\.\\AeroVirtioNetDiag"

/*
 * IOCTLs.
 *
 * AEROVNET_DIAG_IOCTL_QUERY is:
 *   CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS)
 *
 * Defined as a literal so this header stays WDK-free and can be included by the
 * guest selftest without bringing in winioctl.h.
 */
#define AEROVNET_DIAG_IOCTL_QUERY 0x00226000u

/* AEROVNET_DIAG_INFO.InterruptMode values. */
#define AEROVNET_INTERRUPT_MODE_INTX 0u
#define AEROVNET_INTERRUPT_MODE_MSI 1u

#define AEROVNET_DIAG_INFO_VERSION 1u

/*
 * Flags for AEROVNET_DIAG_INFO.Flags.
 *
 * These are best-effort and may change across driver versions; log scrapers
 * should prefer the explicit fields when available.
 */
#define AEROVNET_DIAG_FLAG_USE_MSIX 0x00000001u
#define AEROVNET_DIAG_FLAG_MSIX_ALL_ON_VECTOR0 0x00000002u
#define AEROVNET_DIAG_FLAG_SURPRISE_REMOVED 0x00000004u
#define AEROVNET_DIAG_FLAG_ADAPTER_RUNNING 0x00000008u
#define AEROVNET_DIAG_FLAG_ADAPTER_PAUSED 0x00000010u

#pragma pack(push, 1)
typedef struct _AEROVNET_DIAG_INFO {
    uint32_t Version;
    uint32_t Size;

    uint64_t HostFeatures;
    uint64_t GuestFeatures;

    uint32_t InterruptMode;
    uint32_t MessageCount;

    uint16_t MsixConfigVector;
    uint16_t MsixRxVector;
    uint16_t MsixTxVector;

    uint16_t RxQueueSize;
    uint16_t TxQueueSize;

    /* virtqueue indices (best-effort, snapshot). */
    uint16_t RxAvailIdx;
    uint16_t RxUsedIdx;
    uint16_t TxAvailIdx;
    uint16_t TxUsedIdx;

    uint32_t Flags;

    /* Offload support + enablement. */
    uint8_t TxChecksumSupported;
    uint8_t TxTsoV4Supported;
    uint8_t TxTsoV6Supported;
    uint8_t TxChecksumV4Enabled;
    uint8_t TxChecksumV6Enabled;
    uint8_t TxTsoV4Enabled;
    uint8_t TxTsoV6Enabled;
    uint8_t Reserved0;

    uint64_t StatTxPackets;
    uint64_t StatTxBytes;
    uint64_t StatRxPackets;
    uint64_t StatRxBytes;
    uint64_t StatTxErrors;
    uint64_t StatRxErrors;
    uint64_t StatRxNoBuffers;

    uint32_t RxVqErrorFlags;
    uint32_t TxVqErrorFlags;

    /* TX offload configuration (NDIS-controlled). */
    uint32_t TxTsoMaxOffloadSize;
    uint8_t TxUdpChecksumV4Enabled;
    uint8_t TxUdpChecksumV6Enabled;
    uint8_t Reserved1;
    uint8_t Reserved2;

    /* Optional virtio-net control virtqueue (when VIRTIO_NET_F_CTRL_VQ is negotiated). */
    uint8_t CtrlVqNegotiated;
    uint8_t CtrlRxNegotiated;
    uint8_t CtrlVlanNegotiated;
    uint8_t CtrlMacAddrNegotiated;

    uint16_t CtrlVqQueueIndex;
    uint16_t CtrlVqQueueSize;
    uint32_t CtrlVqErrorFlags;

    uint64_t CtrlCmdSent;
    uint64_t CtrlCmdOk;
    uint64_t CtrlCmdErr;
    uint64_t CtrlCmdTimeout;

    uint64_t StatTxTcpCsumOffload;
    uint64_t StatTxTcpCsumFallback;
    uint64_t StatTxUdpCsumOffload;
    uint64_t StatTxUdpCsumFallback;
} AEROVNET_DIAG_INFO;
#pragma pack(pop)
