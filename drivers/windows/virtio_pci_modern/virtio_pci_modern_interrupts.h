#pragma once

/*
 * Virtio PCI (Modern) interrupt handling helpers for Windows 7 KMDF.
 *
 * Concurrency notes:
 * - With MSI-X multi-vector enabled, KMDF can run multiple interrupt DPCs
 *   concurrently on different CPUs. Do not rely on framework serialization.
 * - Per-queue state must be protected with a per-queue spinlock.
 * - Any access that writes common_cfg.queue_select and then accesses queue-
 *   specific fields must be serialized with a global common_cfg spinlock.
 *
 * See: docs/windows/virtio-pci-modern-interrupts.md (MSI-X concurrency section)
 */

#include <ntddk.h>
#include <wdf.h>

#ifdef __cplusplus
extern "C" {
#endif

#define VIRTIO_MSI_NO_VECTOR ((USHORT)0xFFFF)

#pragma pack(push, 1)

typedef struct _VIRTIO_PCI_COMMON_CFG {
    USHORT device_feature_select;
    USHORT device_feature;
    USHORT driver_feature_select;
    USHORT driver_feature;
    USHORT msix_config;
    USHORT num_queues;
    UCHAR device_status;
    UCHAR config_generation;
    USHORT queue_select;
    USHORT queue_size;
    USHORT queue_msix_vector;
    USHORT queue_enable;
    USHORT queue_notify_off;
    ULONGLONG queue_desc;
    ULONGLONG queue_driver;
    ULONGLONG queue_device;
} VIRTIO_PCI_COMMON_CFG, *PVIRTIO_PCI_COMMON_CFG;

typedef struct _VIRTQ_USED_ELEM {
    UINT32 id;
    UINT32 len;
} VIRTQ_USED_ELEM, *PVIRTQ_USED_ELEM;

typedef struct _VIRTQ_USED {
    UINT16 flags;
    UINT16 idx;
    VIRTQ_USED_ELEM ring[1]; /* Variable length. */
} VIRTQ_USED, *PVIRTQ_USED;

#pragma pack(pop)

typedef struct _VIRTIO_QUEUE VIRTIO_QUEUE, *PVIRTIO_QUEUE;

typedef VOID EVT_VIRTIO_QUEUE_USED(
    _Inout_ PVIRTIO_QUEUE Queue,
    _In_ UINT32 UsedId,
    _In_ UINT32 UsedLen,
    _In_opt_ PVOID Context
);

typedef struct _VIRTIO_QUEUE {
    USHORT QueueIndex;
    USHORT QueueSize;

    /*
     * Guards all queue state and used-ring draining.
     * Must be acquired by the queue DPC before touching queue state.
     */
    WDFSPINLOCK Lock;

    volatile VIRTQ_USED* UsedRing;
    USHORT LastUsedIdx;

    USHORT MsixVector;

    EVT_VIRTIO_QUEUE_USED* EvtUsed;
    PVOID EvtUsedContext;
} VIRTIO_QUEUE, *PVIRTIO_QUEUE;

typedef struct _VIRTIO_PCI_DEVICE_CONTEXT {
    PVIRTIO_PCI_COMMON_CFG CommonCfg;

    /*
     * Serializes any access sequence involving:
     *   common_cfg.queue_select + queue-specific common_cfg fields.
     */
    WDFSPINLOCK CommonCfgLock;

    /*
     * Set to 1 during reset / vector reprogramming to make DPC paths bail out.
     */
    volatile LONG ResetInProgress;

    PVIRTIO_QUEUE Queues;
    ULONG QueueCount;

    WDFINTERRUPT* Interrupts;
    ULONG InterruptCount;

    USHORT ConfigMsixVector;
} VIRTIO_PCI_DEVICE_CONTEXT, *PVIRTIO_PCI_DEVICE_CONTEXT;

typedef enum _VIRTIO_INTERRUPT_KIND {
    VirtioInterruptKindConfig = 0,
    VirtioInterruptKindQueue = 1,
} VIRTIO_INTERRUPT_KIND;

typedef struct _VIRTIO_INTERRUPT_CONTEXT {
    PVIRTIO_PCI_DEVICE_CONTEXT DeviceContext;
    VIRTIO_INTERRUPT_KIND Kind;
    PVIRTIO_QUEUE Queue; /* Only for VirtioInterruptKindQueue. */
    USHORT MsixVector;
} VIRTIO_INTERRUPT_CONTEXT, *PVIRTIO_INTERRUPT_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_INTERRUPT_CONTEXT, VirtioPciGetInterruptContext);

EVT_WDF_INTERRUPT_ISR VirtioPciModernEvtInterruptIsr;
EVT_WDF_INTERRUPT_DPC VirtioPciModernEvtInterruptDpc;

NTSTATUS VirtioPciModernInitializeLocks(_In_ WDFDEVICE Device, _Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx);

NTSTATUS VirtioPciModernCreateInterrupt(
    _In_ WDFDEVICE Device,
    _Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx,
    _In_ VIRTIO_INTERRUPT_KIND Kind,
    _In_opt_ PVIRTIO_QUEUE Queue,
    _In_ USHORT MsixVector,
    _In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptRaw,
    _In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptTranslated,
    _Out_ WDFINTERRUPT* InterruptOut
);

NTSTATUS VirtioPciModernQuiesceInterrupts(_Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx);
NTSTATUS VirtioPciModernResumeInterrupts(_Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx);

NTSTATUS VirtioPciModernProgramMsixVectors(
    _Inout_ PVIRTIO_PCI_DEVICE_CONTEXT DevCtx,
    _In_ USHORT ConfigVector,
    _In_reads_(DevCtx->QueueCount) const USHORT* QueueVectors
);

#ifdef __cplusplus
} /* extern "C" */
#endif
