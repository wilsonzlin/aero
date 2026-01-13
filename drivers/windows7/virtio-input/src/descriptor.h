#pragma once

#include <ntddk.h>
#include <hidport.h>

#define VIRTIO_INPUT_VID 0x1AF4
#define VIRTIO_INPUT_PID_KEYBOARD 0x0001
#define VIRTIO_INPUT_PID_MOUSE 0x0002
#define VIRTIO_INPUT_PID_TABLET 0x0003
#define VIRTIO_INPUT_VERSION 0x0001

extern const UCHAR VirtioInputKeyboardReportDescriptor[];
extern const USHORT VirtioInputKeyboardReportDescriptorLength;
extern const HID_DESCRIPTOR VirtioInputKeyboardHidDescriptor;

extern const UCHAR VirtioInputMouseReportDescriptor[];
extern const USHORT VirtioInputMouseReportDescriptorLength;
extern const HID_DESCRIPTOR VirtioInputMouseHidDescriptor;

extern const UCHAR VirtioInputTabletReportDescriptor[];
extern const USHORT VirtioInputTabletReportDescriptorLength;
extern const HID_DESCRIPTOR VirtioInputTabletHidDescriptor;

PCWSTR VirtioInputGetManufacturerString(void);
PCWSTR VirtioInputGetKeyboardProductString(void);
PCWSTR VirtioInputGetMouseProductString(void);
PCWSTR VirtioInputGetTabletProductString(void);
PCWSTR VirtioInputGetSerialString(void);
