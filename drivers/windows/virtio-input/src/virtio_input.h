#pragma once

#include <ntddk.h>
#include <wdf.h>
#include <hidport.h>

#define VIRTIOINPUT_POOL_TAG 'pInV'

typedef struct _DEVICE_CONTEXT {
    WDFQUEUE DefaultQueue;
} DEVICE_CONTEXT, *PDEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, VirtioInputGetDeviceContext);

EVT_WDF_DRIVER_DEVICE_ADD VirtioInputEvtDriverDeviceAdd;
EVT_WDF_DEVICE_PREPARE_HARDWARE VirtioInputEvtDevicePrepareHardware;
EVT_WDF_DEVICE_RELEASE_HARDWARE VirtioInputEvtDeviceReleaseHardware;
EVT_WDF_DEVICE_D0_ENTRY VirtioInputEvtDeviceD0Entry;
EVT_WDF_DEVICE_D0_EXIT VirtioInputEvtDeviceD0Exit;

EVT_WDF_IO_QUEUE_IO_INTERNAL_DEVICE_CONTROL VirtioInputEvtIoInternalDeviceControl;

NTSTATUS VirtioInputQueueInitialize(_In_ WDFDEVICE Device);

