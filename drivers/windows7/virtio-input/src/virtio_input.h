#pragma once

/*
 * Minimal virtio-input device glue for the HID translation layer.
 *
 * The real KMDF driver is expected to:
 *   - Provide virtqueue consumption (DMA buffers + interrupt/DPC scheduling).
 *   - Call virtio_input_process_event_le() for each received event.
 *   - Satisfy IOCTL_HID_READ_REPORT by popping from the report ring and/or
 *     completing pending reads when reports arrive.
 *
 * This file keeps that interface small and unit-test friendly.
 */

#include "hid_translate.h"

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
  VIRTIO_INPUT_REPORT_MAX_SIZE = HID_TRANSLATE_KEYBOARD_REPORT_SIZE,
  VIRTIO_INPUT_REPORT_RING_CAPACITY = 128,
};

struct virtio_input_report {
  uint8_t len;
  uint8_t data[VIRTIO_INPUT_REPORT_MAX_SIZE];
};

struct virtio_input_report_ring {
  struct virtio_input_report reports[VIRTIO_INPUT_REPORT_RING_CAPACITY];
  uint32_t head;
  uint32_t tail;
  uint32_t count;
};

typedef void (*virtio_input_report_ready_fn)(void *context);
typedef void (*virtio_input_lock_fn)(void *context);

struct virtio_input_device {
  struct hid_translate translate;
  struct virtio_input_report_ring report_ring;

  virtio_input_lock_fn lock;
  virtio_input_lock_fn unlock;
  void *lock_context;

  virtio_input_report_ready_fn report_ready;
  void *report_ready_context;
};

void virtio_input_device_init(struct virtio_input_device *dev, virtio_input_report_ready_fn report_ready,
                              void *report_ready_context, virtio_input_lock_fn lock, virtio_input_lock_fn unlock,
                              void *lock_context);

void virtio_input_device_set_enabled_reports(struct virtio_input_device *dev, uint8_t enabled_reports);

void virtio_input_device_reset_state(struct virtio_input_device *dev, bool emit_reports);

void virtio_input_process_event_le(struct virtio_input_device *dev, const struct virtio_input_event_le *ev_le);

/*
 * Pops the next queued HID report (oldest first). Returns true if a report was
 * returned, false if the ring is empty.
 */
bool virtio_input_try_pop_report(struct virtio_input_device *dev, struct virtio_input_report *out_report);

#ifdef _WIN32
#include <hidport.h>
#include <ntddk.h>
#include <wdf.h>

#include "virtio_statusq.h"
#include "virtio_pci_interrupts.h"
#include "virtio_pci_modern.h"

#ifndef HID_HID_DESCRIPTOR_TYPE
#define HID_HID_DESCRIPTOR_TYPE 0x21
#endif

#ifndef HID_REPORT_DESCRIPTOR_TYPE
#define HID_REPORT_DESCRIPTOR_TYPE 0x22
#endif
#include "log.h"

__forceinline VOID VioInputMdlFree(_Inout_opt_ PMDL *Mdl)
{
    if (Mdl == NULL || *Mdl == NULL) {
        return;
    }

    MmUnlockPages(*Mdl);
    IoFreeMdl(*Mdl);
    *Mdl = NULL;
}

__forceinline NTSTATUS VioInputMapUserAddress(
    _In_ PVOID UserAddress,
    _In_ SIZE_T Length,
    _In_ LOCK_OPERATION Operation,
    _Outptr_ PMDL *MdlOut,
    _Outptr_result_bytebuffer_(Length) PVOID *SystemAddressOut
)
{
    PMDL mdl;
    PVOID systemAddress;

    if (MdlOut == NULL || SystemAddressOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *MdlOut = NULL;
    *SystemAddressOut = NULL;

    if (UserAddress == NULL || Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    // IoAllocateMdl takes an ULONG length.
    if (Length > (SIZE_T)MAXULONG) {
        return STATUS_INVALID_PARAMETER;
    }

    mdl = IoAllocateMdl(UserAddress, (ULONG)Length, FALSE, FALSE, NULL);
    if (mdl == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    __try {
        MmProbeAndLockPages(mdl, UserMode, Operation);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        IoFreeMdl(mdl);
        return (NTSTATUS)GetExceptionCode();
    }

    systemAddress = MmGetSystemAddressForMdlSafe(mdl, NormalPagePriority);
    if (systemAddress == NULL) {
        MmUnlockPages(mdl);
        IoFreeMdl(mdl);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    *MdlOut = mdl;
    *SystemAddressOut = systemAddress;
    return STATUS_SUCCESS;
}

__forceinline NTSTATUS VioInputReadRequestInputUlong(_In_ WDFREQUEST Request, _Out_ ULONG *ValueOut)
{
    NTSTATUS status;
    ULONG *userPtr;
    size_t len;

    if (ValueOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *ValueOut = 0;

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(ULONG), (PVOID *)&userPtr, &len);
    if (!NT_SUCCESS(status) || len < sizeof(ULONG)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (WdfRequestGetRequestorMode(Request) == UserMode) {
        PMDL mdl;
        ULONG *systemPtr;

        mdl = NULL;
        systemPtr = NULL;
        status = VioInputMapUserAddress(userPtr, sizeof(ULONG), IoReadAccess, &mdl, (PVOID *)&systemPtr);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        *ValueOut = *(UNALIGNED ULONG *)systemPtr;
        VioInputMdlFree(&mdl);
        return STATUS_SUCCESS;
    }

    *ValueOut = *(UNALIGNED ULONG *)userPtr;
    return STATUS_SUCCESS;
}

#define VIRTIOINPUT_POOL_TAG 'pInV'

#define VIRTIO_INPUT_REPORT_ID_ANY 0
#define VIRTIO_INPUT_REPORT_ID_KEYBOARD HID_TRANSLATE_REPORT_ID_KEYBOARD
#define VIRTIO_INPUT_REPORT_ID_MOUSE HID_TRANSLATE_REPORT_ID_MOUSE
#define VIRTIO_INPUT_MAX_REPORT_ID VIRTIO_INPUT_REPORT_ID_MOUSE

#define VIRTIO_INPUT_KBD_INPUT_REPORT_SIZE HID_TRANSLATE_KEYBOARD_REPORT_SIZE
#define VIRTIO_INPUT_MOUSE_INPUT_REPORT_SIZE HID_TRANSLATE_MOUSE_REPORT_SIZE

typedef enum _VIOINPUT_DEVICE_KIND {
    VioInputDeviceKindUnknown = 0,
    VioInputDeviceKindKeyboard,
    VioInputDeviceKindMouse,
} VIOINPUT_DEVICE_KIND;

#define VIOINPUT_PCI_SUBSYSTEM_ID_KEYBOARD 0x0010
#define VIOINPUT_PCI_SUBSYSTEM_ID_MOUSE 0x0011

/*
 * Forward declaration for the shared virtqueue implementation (drivers/windows/virtio/common).
 */
typedef struct _VIRTQ_SPLIT VIRTQ_SPLIT;

typedef struct _VIRTIO_INPUT_FILE_CONTEXT {
    ULONG CollectionNumber;
    UCHAR DefaultReportId;
    BOOLEAN HasCollectionEa;
} VIRTIO_INPUT_FILE_CONTEXT, *PVIRTIO_INPUT_FILE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_INPUT_FILE_CONTEXT, VirtioInputGetFileContext);

enum { VIRTIO_INPUT_QUEUE_COUNT = 2 };

typedef struct _DEVICE_CONTEXT {
    WDFQUEUE DefaultQueue;
    WDFQUEUE PendingReadQueue;
    WDFSPINLOCK InputLock;
    WDFWORKITEM ReadWorkItem;
    struct virtio_input_device InputDevice;
    // Manual read queues indexed by ReportID. Index 0 is a special "any report"
    // queue used for non-collection (parent interface) opens.
    WDFQUEUE ReadReportQueue[VIRTIO_INPUT_MAX_REPORT_ID + 1];
    WDFSPINLOCK ReadReportLock;
    WDFWAITLOCK ReadReportWaitLock;
    BOOLEAN ReadReportsEnabled;
    struct virtio_input_report_ring PendingReportRing[VIRTIO_INPUT_MAX_REPORT_ID + 1];

    PVIRTIO_STATUSQ StatusQ;
    VIRTQ_SPLIT* EventVq;
    WDFCOMMONBUFFER EventRingCommonBuffer;
    WDFCOMMONBUFFER EventRxCommonBuffer;
    PVOID EventRxVa;
    UINT64 EventRxPa;
    USHORT EventQueueSize;

    VIOINPUT_COUNTERS Counters;
    VIRTIO_PCI_MODERN_DEVICE PciDevice;
    volatile UINT16* QueueNotifyAddrCache[VIRTIO_INPUT_QUEUE_COUNT];
    WDFDMAENABLER DmaEnabler;
    UINT64 NegotiatedFeatures;

    BOOLEAN HardwareReady;
    BOOLEAN InD0;
    BOOLEAN HidActivated;
    volatile LONG VirtioStarted;
    ULONG NumDeviceInputBuffers;
    VIOINPUT_DEVICE_KIND DeviceKind;
    USHORT PciSubsystemDeviceId;
    UCHAR PciRevisionId;

    VIRTIO_PCI_INTERRUPTS Interrupts;

    volatile LONG ConfigInterruptCount;
    volatile LONG QueueInterruptCount[VIRTIO_INPUT_QUEUE_COUNT];
} DEVICE_CONTEXT, *PDEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, VirtioInputGetDeviceContext);

__forceinline BOOLEAN VirtioInputIsHidActive(_In_ const DEVICE_CONTEXT* Ctx)
{
    return Ctx->HardwareReady && Ctx->InD0 && Ctx->HidActivated;
}

EVT_WDF_DRIVER_DEVICE_ADD VirtioInputEvtDriverDeviceAdd;
EVT_WDF_DEVICE_PREPARE_HARDWARE VirtioInputEvtDevicePrepareHardware;
EVT_WDF_DEVICE_RELEASE_HARDWARE VirtioInputEvtDeviceReleaseHardware;
EVT_WDF_DEVICE_D0_ENTRY VirtioInputEvtDeviceD0Entry;
EVT_WDF_DEVICE_D0_EXIT VirtioInputEvtDeviceD0Exit;

EVT_WDF_IO_QUEUE_IO_INTERNAL_DEVICE_CONTROL VirtioInputEvtIoInternalDeviceControl;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL VirtioInputEvtIoDeviceControl;

NTSTATUS VirtioInputQueueInitialize(_In_ WDFDEVICE Device);
NTSTATUS VirtioInputFileConfigure(_Inout_ WDFDEVICE_INIT *DeviceInit);
NTSTATUS VirtioInputReadReportQueuesInitialize(_In_ WDFDEVICE Device);
VOID VirtioInputReadReportQueuesStart(_In_ WDFDEVICE Device);
VOID VirtioInputReadReportQueuesStopAndFlush(_In_ WDFDEVICE Device, _In_ NTSTATUS CompletionStatus);

NTSTATUS VirtioInputHandleHidIoctl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode);

NTSTATUS VirtioInputHandleHidReadReport(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request, _In_ size_t OutputBufferLength);
NTSTATUS VirtioInputHandleHidWriteReport(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request, _In_ size_t InputBufferLength);
NTSTATUS VirtioInputReportArrived(
    _In_ WDFDEVICE Device,
    _In_ UCHAR ReportId,
    _In_reads_bytes_(ReportSize) const VOID *Report,
    _In_ size_t ReportSize);

NTSTATUS VirtioInputHidActivateDevice(_In_ WDFDEVICE Device);
NTSTATUS VirtioInputHidDeactivateDevice(_In_ WDFDEVICE Device);
VOID VirtioInputHidFlushQueue(_In_ WDFDEVICE Device);
#endif /* _WIN32 */

#ifdef __cplusplus
} /* extern "C" */
#endif
