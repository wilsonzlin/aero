/**
 * USB HID report descriptors for Aero's built-in synthetic input devices.
 *
 * These bytes are mirrored from the canonical Rust implementations in `crates/aero-usb`:
 * - `hid/keyboard.rs`
 * - `hid/mouse.rs`
 * - `hid/gamepad.rs`
 *
 * Keep these in sync with `UsbHidBridge.drain_next_*_report()` report formats.
 */

export const USB_HID_BOOT_KEYBOARD_REPORT_DESCRIPTOR = new Uint8Array([
  0x05, 0x01, // Usage Page (Generic Desktop)
  0x09, 0x06, // Usage (Keyboard)
  0xa1, 0x01, // Collection (Application)
  0x05, 0x07, // Usage Page (Keyboard/Keypad)
  0x19, 0xe0, // Usage Minimum (Left Control)
  0x29, 0xe7, // Usage Maximum (Right GUI)
  0x15, 0x00, // Logical Minimum (0)
  0x25, 0x01, // Logical Maximum (1)
  0x75, 0x01, // Report Size (1)
  0x95, 0x08, // Report Count (8)
  0x81, 0x02, // Input (Data,Var,Abs) Modifier byte
  0x95, 0x01, // Report Count (1)
  0x75, 0x08, // Report Size (8)
  0x81, 0x01, // Input (Const,Array,Abs) Reserved byte
  0x95, 0x05, // Report Count (5)
  0x75, 0x01, // Report Size (1)
  0x05, 0x08, // Usage Page (LEDs)
  0x19, 0x01, // Usage Minimum (Num Lock)
  0x29, 0x05, // Usage Maximum (Kana)
  0x91, 0x02, // Output (Data,Var,Abs) LED report
  0x95, 0x01, // Report Count (1)
  0x75, 0x03, // Report Size (3)
  0x91, 0x01, // Output (Const,Array,Abs) LED padding
  0x95, 0x06, // Report Count (6)
  0x75, 0x08, // Report Size (8)
  0x15, 0x00, // Logical Minimum (0)
  0x25, 0x89, // Logical Maximum (137)
  0x05, 0x07, // Usage Page (Keyboard/Keypad)
  0x19, 0x00, // Usage Minimum (0)
  0x29, 0x89, // Usage Maximum (137)
  0x81, 0x00, // Input (Data,Array,Abs) Key arrays (6 bytes)
  0xc0, // End Collection
]);

export const USB_HID_BOOT_MOUSE_REPORT_DESCRIPTOR = new Uint8Array([
  0x05, 0x01, // Usage Page (Generic Desktop)
  0x09, 0x02, // Usage (Mouse)
  0xa1, 0x01, // Collection (Application)
  0x09, 0x01, // Usage (Pointer)
  0xa1, 0x00, // Collection (Physical)
  0x05, 0x09, // Usage Page (Buttons)
  0x19, 0x01, // Usage Minimum (Button 1)
  0x29, 0x03, // Usage Maximum (Button 3)
  0x15, 0x00, // Logical Minimum (0)
  0x25, 0x01, // Logical Maximum (1)
  0x95, 0x03, // Report Count (3)
  0x75, 0x01, // Report Size (1)
  0x81, 0x02, // Input (Data,Var,Abs) Button bits
  0x95, 0x01, // Report Count (1)
  0x75, 0x05, // Report Size (5)
  0x81, 0x01, // Input (Const,Array,Abs) Padding
  0x05, 0x01, // Usage Page (Generic Desktop)
  0x09, 0x30, // Usage (X)
  0x09, 0x31, // Usage (Y)
  0x09, 0x38, // Usage (Wheel)
  0x15, 0x81, // Logical Minimum (-127)
  0x25, 0x7f, // Logical Maximum (127)
  0x75, 0x08, // Report Size (8)
  0x95, 0x03, // Report Count (3)
  0x81, 0x06, // Input (Data,Var,Rel) X,Y,Wheel
  0xc0, // End Collection
  0xc0, // End Collection
]);

export const USB_HID_GAMEPAD_REPORT_DESCRIPTOR = new Uint8Array([
  0x05, 0x01, // Usage Page (Generic Desktop)
  0x09, 0x05, // Usage (Game Pad)
  0xa1, 0x01, // Collection (Application)
  0x05, 0x09, // Usage Page (Button)
  0x19, 0x01, // Usage Minimum (Button 1)
  0x29, 0x10, // Usage Maximum (Button 16)
  0x15, 0x00, // Logical Minimum (0)
  0x25, 0x01, // Logical Maximum (1)
  0x75, 0x01, // Report Size (1)
  0x95, 0x10, // Report Count (16)
  0x81, 0x02, // Input (Data,Var,Abs) Buttons
  0x05, 0x01, // Usage Page (Generic Desktop)
  0x09, 0x39, // Usage (Hat switch)
  0x15, 0x00, // Logical Minimum (0)
  0x25, 0x07, // Logical Maximum (7)
  0x35, 0x00, // Physical Minimum (0)
  0x46, 0x3b, 0x01, // Physical Maximum (315)
  0x65, 0x14, // Unit (Eng Rot: Degrees)
  0x75, 0x04, // Report Size (4)
  0x95, 0x01, // Report Count (1)
  0x81, 0x42, // Input (Data,Var,Abs,Null) Hat
  0x65, 0x00, // Unit (None)
  0x75, 0x04, // Report Size (4)
  0x95, 0x01, // Report Count (1)
  0x81, 0x01, // Input (Const,Array,Abs) Padding
  0x09, 0x30, // Usage (X)
  0x09, 0x31, // Usage (Y)
  0x09, 0x33, // Usage (Rx)
  0x09, 0x34, // Usage (Ry)
  0x15, 0x81, // Logical Minimum (-127)
  0x25, 0x7f, // Logical Maximum (127)
  0x75, 0x08, // Report Size (8)
  0x95, 0x04, // Report Count (4)
  0x81, 0x02, // Input (Data,Var,Abs) Axes
  0x75, 0x08, // Report Size (8)
  0x95, 0x01, // Report Count (1)
  0x81, 0x01, // Input (Const,Array,Abs) Padding
  0xc0, // End Collection
]);

export const USB_HID_INTERFACE_SUBCLASS_BOOT = 0x01;
export const USB_HID_INTERFACE_PROTOCOL_KEYBOARD = 0x01;
export const USB_HID_INTERFACE_PROTOCOL_MOUSE = 0x02;
