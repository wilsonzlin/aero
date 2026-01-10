#pragma once

#include <ntddk.h>
#include <hidport.h>

#define VIRTIO_INPUT_VID 0x1AF4
#define VIRTIO_INPUT_PID 0x1052
#define VIRTIO_INPUT_VERSION 0x0001

extern const UCHAR VirtioInputReportDescriptor[];
extern const USHORT VirtioInputReportDescriptorLength;
extern const HID_DESCRIPTOR VirtioInputHidDescriptor;

PCWSTR VirtioInputGetManufacturerString(void);
PCWSTR VirtioInputGetProductString(void);
PCWSTR VirtioInputGetSerialString(void);

