#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"

#define VIRTIOSND_POOL_TAG 'dnSV' // 'VSnd' (endianness depends on debugger display)

#define VIRTIOSND_MAX_MMIO_RANGES 6

typedef struct _VIRTIOSND_MMIO_RANGE {
    PHYSICAL_ADDRESS PhysicalAddress;
    ULONG Length;
    PVOID BaseAddress;
} VIRTIOSND_MMIO_RANGE, *PVIRTIOSND_MMIO_RANGE;

typedef struct _VIRTIOSND_DEVICE_EXTENSION {
    PDEVICE_OBJECT Self;
    PDEVICE_OBJECT LowerDeviceObject;

    IO_REMOVE_LOCK RemoveLock;

    ULONG MmioRangeCount;
    VIRTIOSND_MMIO_RANGE MmioRanges[VIRTIOSND_MAX_MMIO_RANGES];

    BOOLEAN HasIoPort;
    ULONG_PTR IoPortBase;
    ULONG IoPortLength;

    BOOLEAN Started;
    BOOLEAN Removed;
} VIRTIOSND_DEVICE_EXTENSION, *PVIRTIOSND_DEVICE_EXTENSION;

#define VIRTIOSND_GET_DX(_DeviceObject) ((PVIRTIOSND_DEVICE_EXTENSION)(_DeviceObject)->DeviceExtension)
