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
#include <stdint.h>

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

struct virtio_input_device {
  struct hid_translate translate;
  struct virtio_input_report_ring report_ring;

  virtio_input_report_ready_fn report_ready;
  void *report_ready_context;
};

void virtio_input_device_init(struct virtio_input_device *dev, virtio_input_report_ready_fn report_ready,
                              void *report_ready_context);

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

#define VIRTIOINPUT_POOL_TAG 'pInV'

typedef struct _DEVICE_CONTEXT {
    WDFQUEUE DefaultQueue;
    struct virtio_input_device InputDevice;
} DEVICE_CONTEXT, *PDEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, VirtioInputGetDeviceContext);

EVT_WDF_DRIVER_DEVICE_ADD VirtioInputEvtDriverDeviceAdd;
EVT_WDF_DEVICE_PREPARE_HARDWARE VirtioInputEvtDevicePrepareHardware;
EVT_WDF_DEVICE_RELEASE_HARDWARE VirtioInputEvtDeviceReleaseHardware;
EVT_WDF_DEVICE_D0_ENTRY VirtioInputEvtDeviceD0Entry;
EVT_WDF_DEVICE_D0_EXIT VirtioInputEvtDeviceD0Exit;

EVT_WDF_IO_QUEUE_IO_INTERNAL_DEVICE_CONTROL VirtioInputEvtIoInternalDeviceControl;

NTSTATUS VirtioInputQueueInitialize(_In_ WDFDEVICE Device);
#endif /* _WIN32 */

#ifdef __cplusplus
} /* extern "C" */
#endif
