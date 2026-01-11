#pragma once

#include <ntddk.h>
#include <hidport.h>

#define VIRTIO_INPUT_VID 0x1AF4
#define VIRTIO_INPUT_PID_KEYBOARD 0x0001
#define VIRTIO_INPUT_PID_MOUSE 0x0002
#define VIRTIO_INPUT_VERSION 0x0001

extern const UCHAR VirtioInputKeyboardReportDescriptor[];
extern const USHORT VirtioInputKeyboardReportDescriptorLength;
extern const HID_DESCRIPTOR VirtioInputKeyboardHidDescriptor;

extern const UCHAR VirtioInputMouseReportDescriptor[];
extern const USHORT VirtioInputMouseReportDescriptorLength;
extern const HID_DESCRIPTOR VirtioInputMouseHidDescriptor;

PCWSTR VirtioInputGetManufacturerString(void);
PCWSTR VirtioInputGetKeyboardProductString(void);
PCWSTR VirtioInputGetMouseProductString(void);
PCWSTR VirtioInputGetSerialString(void);
