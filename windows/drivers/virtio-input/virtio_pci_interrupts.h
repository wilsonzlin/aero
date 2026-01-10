#ifndef VIRTIO_PCI_INTERRUPTS_H_
#define VIRTIO_PCI_INTERRUPTS_H_

#include <ntddk.h>
#include <wdf.h>

#define VIRTIO_PCI_ISR_QUEUE_INTERRUPT  0x01
#define VIRTIO_PCI_ISR_CONFIG_INTERRUPT 0x02

typedef struct _VIRTIO_PCI_COMMON_CFG {
    ULONG  device_feature_select;
    ULONG  device_feature;
    ULONG  driver_feature_select;
    ULONG  driver_feature;
    USHORT msix_config;
    USHORT num_queues;
    UCHAR  device_status;
    UCHAR  config_generation;
    USHORT queue_select;
    USHORT queue_size;
    USHORT queue_msix_vector;
    USHORT queue_enable;
    USHORT queue_notify_off;
    ULONGLONG queue_desc;
    ULONGLONG queue_avail;
    ULONGLONG queue_used;
} VIRTIO_PCI_COMMON_CFG, *PVIRTIO_PCI_COMMON_CFG;

typedef enum _VIRTIO_PCI_INTERRUPT_MODE {
    VirtioPciInterruptModeUnknown = 0,
    VirtioPciInterruptModeIntx,
    VirtioPciInterruptModeMsix,
} VIRTIO_PCI_INTERRUPT_MODE;

typedef VOID EVT_VIRTIO_PCI_CONFIG_CHANGE(
    _In_ WDFDEVICE Device,
    _In_opt_ PVOID Context
    );

typedef VOID EVT_VIRTIO_PCI_DRAIN_QUEUE(
    _In_ WDFDEVICE Device,
    _In_ ULONG QueueIndex,
    _In_opt_ PVOID Context
    );

typedef struct _VIRTIO_PCI_INTERRUPTS {
    VIRTIO_PCI_INTERRUPT_MODE Mode;

    ULONG QueueCount;
    volatile UCHAR* IsrStatusRegister;

    EVT_VIRTIO_PCI_CONFIG_CHANGE* EvtConfigChange;
    EVT_VIRTIO_PCI_DRAIN_QUEUE* EvtDrainQueue;
    PVOID CallbackContext;

    WDFSPINLOCK* QueueLocks;
    WDFMEMORY QueueLocksMemory;

    union {
        struct {
            WDFINTERRUPT Interrupt;
            volatile LONG PendingIsrStatus;
            volatile LONG SpuriousCount;
        } Intx;

        struct {
            ULONG MessageCount;
            USHORT UsedVectorCount;
            USHORT ConfigVector;
            WDFINTERRUPT* Interrupts;
            WDFMEMORY InterruptsMemory;
            USHORT* QueueVectors;
            WDFMEMORY QueueVectorsMemory;
        } Msix;
    } u;
} VIRTIO_PCI_INTERRUPTS, *PVIRTIO_PCI_INTERRUPTS;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciInterruptsPrepareHardware(
    _In_ WDFDEVICE Device,
    _Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated,
    _In_ ULONG QueueCount,
    _In_ volatile UCHAR* IsrStatusRegister,
    _In_opt_ EVT_VIRTIO_PCI_CONFIG_CHANGE* EvtConfigChange,
    _In_opt_ EVT_VIRTIO_PCI_DRAIN_QUEUE* EvtDrainQueue,
    _In_opt_ PVOID CallbackContext
    );

VOID VirtioPciInterruptsReleaseHardware(_Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciProgramMsixVectors(
    _In_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg,
    _In_ ULONG QueueCount,
    _In_ USHORT ConfigVector,
    _In_reads_(QueueCount) const USHORT* QueueVectors
    );

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciInterruptsProgramMsixVectors(
    _In_ const PVIRTIO_PCI_INTERRUPTS Interrupts,
    _In_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg
    );

#endif

