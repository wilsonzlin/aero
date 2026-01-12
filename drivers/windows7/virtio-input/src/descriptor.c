#include "descriptor.h"

const UCHAR VirtioInputKeyboardReportDescriptor[] = {
    //
    // Report ID 1: Keyboard (8 modifier bits + reserved + 6-key rollover)
    // Report ID 1: Keyboard LEDs (output)
    //
    0x05, 0x01,        // Usage Page (Generic Desktop)
    0x09, 0x06,        // Usage (Keyboard)
    0xA1, 0x01,        // Collection (Application)
    0x85, 0x01,        //   Report ID (1)
    0x05, 0x07,        //   Usage Page (Keyboard/Keypad)
    0x19, 0xE0,        //   Usage Minimum (Left Control)
    0x29, 0xE7,        //   Usage Maximum (Right GUI)
    0x15, 0x00,        //   Logical Minimum (0)
    0x25, 0x01,        //   Logical Maximum (1)
    0x75, 0x01,        //   Report Size (1)
    0x95, 0x08,        //   Report Count (8)
    0x81, 0x02,        //   Input (Data,Var,Abs) ; Modifier byte
    0x95, 0x01,        //   Report Count (1)
    0x75, 0x08,        //   Report Size (8)
    0x81, 0x01,        //   Input (Const,Array,Abs) ; Reserved byte
    0x95, 0x06,        //   Report Count (6)
    0x75, 0x08,        //   Report Size (8)
    0x15, 0x00,        //   Logical Minimum (0)
    0x25, 0x89,        //   Logical Maximum (137)
    0x05, 0x07,        //   Usage Page (Keyboard/Keypad)
    0x19, 0x00,        //   Usage Minimum (0)
    0x29, 0x89,        //   Usage Maximum (137)
    0x81, 0x00,        //   Input (Data,Array,Abs) ; 6-key rollover
    0x05, 0x08,        //   Usage Page (LEDs)
    0x19, 0x01,        //   Usage Minimum (Num Lock)
    0x29, 0x05,        //   Usage Maximum (Kana)
    0x95, 0x05,        //   Report Count (5)
    0x75, 0x01,        //   Report Size (1)
    0x91, 0x02,        //   Output (Data,Var,Abs) ; LED report
    0x95, 0x01,        //   Report Count (1)
    0x75, 0x03,        //   Report Size (3)
    0x91, 0x01,        //   Output (Const,Array,Abs) ; Padding
    0xC0,              // End Collection
};

const USHORT VirtioInputKeyboardReportDescriptorLength = (USHORT)sizeof(VirtioInputKeyboardReportDescriptor);

const HID_DESCRIPTOR VirtioInputKeyboardHidDescriptor = {
    (UCHAR)sizeof(HID_DESCRIPTOR),
    HID_HID_DESCRIPTOR_TYPE,
    HID_REVISION,
    0,
    1,
    { HID_REPORT_DESCRIPTOR_TYPE, (USHORT)sizeof(VirtioInputKeyboardReportDescriptor) },
};

const UCHAR VirtioInputMouseReportDescriptor[] = {
    //
    // Report ID 2: Mouse (5 buttons + X/Y/Wheel)
    //
    0x05, 0x01,        // Usage Page (Generic Desktop)
    0x09, 0x02,        // Usage (Mouse)
    0xA1, 0x01,        // Collection (Application)
    0x85, 0x02,        //   Report ID (2)
    0x09, 0x01,        //   Usage (Pointer)
    0xA1, 0x00,        //   Collection (Physical)
    0x05, 0x09,        //     Usage Page (Button)
    0x19, 0x01,        //     Usage Minimum (Button 1)
    0x29, 0x05,        //     Usage Maximum (Button 5)
    0x15, 0x00,        //     Logical Minimum (0)
    0x25, 0x01,        //     Logical Maximum (1)
    0x95, 0x05,        //     Report Count (5)
    0x75, 0x01,        //     Report Size (1)
    0x81, 0x02,        //     Input (Data,Var,Abs) ; Buttons
    0x95, 0x01,        //     Report Count (1)
    0x75, 0x03,        //     Report Size (3)
    0x81, 0x01,        //     Input (Const,Array,Abs) ; Padding
    0x05, 0x01,        //     Usage Page (Generic Desktop)
    0x09, 0x30,        //     Usage (X)
    0x09, 0x31,        //     Usage (Y)
    0x09, 0x38,        //     Usage (Wheel)
    0x15, 0x81,        //     Logical Minimum (-127)
    0x25, 0x7F,        //     Logical Maximum (127)
    0x75, 0x08,        //     Report Size (8)
    0x95, 0x03,        //     Report Count (3)
    0x81, 0x06,        //     Input (Data,Var,Rel) ; X, Y, Wheel
    0xC0,              //   End Collection
    0xC0,              // End Collection
};

const USHORT VirtioInputMouseReportDescriptorLength = (USHORT)sizeof(VirtioInputMouseReportDescriptor);

const HID_DESCRIPTOR VirtioInputMouseHidDescriptor = {
    (UCHAR)sizeof(HID_DESCRIPTOR),
    HID_HID_DESCRIPTOR_TYPE,
    HID_REVISION,
    0,
    1,
    { HID_REPORT_DESCRIPTOR_TYPE, (USHORT)sizeof(VirtioInputMouseReportDescriptor) },
};

static const WCHAR VirtioInputManufacturerString[] = L"Aero";
static const WCHAR VirtioInputKeyboardProductString[] = L"Aero Virtio Keyboard";
static const WCHAR VirtioInputMouseProductString[] = L"Aero Virtio Mouse";
static const WCHAR VirtioInputSerialString[] = L"00000001";

PCWSTR VirtioInputGetManufacturerString(void)
{
    return VirtioInputManufacturerString;
}

PCWSTR VirtioInputGetKeyboardProductString(void)
{
    return VirtioInputKeyboardProductString;
}

PCWSTR VirtioInputGetMouseProductString(void)
{
    return VirtioInputMouseProductString;
}

PCWSTR VirtioInputGetSerialString(void)
{
    return VirtioInputSerialString;
}
