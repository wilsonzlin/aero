export type WebUsbInterfaceDescriptor = {
  configurationValue: number;
  interfaceNumber: number;
  alternateSetting: number;
  classCode: number;
  subclassCode: number;
  protocolCode: number;
};

export type WebUsbDeviceClassification = {
  hasAnyInterfaces: boolean;
  hasUnprotectedInterfaces: boolean;
  protected: WebUsbInterfaceDescriptor[];
  unprotected: WebUsbInterfaceDescriptor[];
};

/**
 * USB interface classes blocked by Chromium's WebUSB "protected interface class"
 * restrictions (see `chrome://usb-internals` and Chromium source).
 *
 * Note: this is intentionally interface-class based (not device class) because
 * WebUSB access is granted per device, but claiming is done per interface.
 */
export const PROTECTED_USB_INTERFACE_CLASSES: ReadonlySet<number> = new Set([
  0x01, // Audio
  0x03, // HID
  0x05, // Physical
  0x06, // Image
  0x07, // Printer
  0x08, // Mass Storage
  0x09, // Hub
  0x0b, // Smart Card
  0x0d, // Content Security
  0x0e, // Video
  0x0f, // Personal Healthcare
  0x10, // Audio/Video
  0x11, // Billboard
  0x12, // USB Type-C Bridge
  0xdc, // Diagnostic Device
  0xe0, // Wireless Controller (e.g. Bluetooth)
]);

export function isProtectedInterfaceClass(classCode: number): boolean {
  return PROTECTED_USB_INTERFACE_CLASSES.has(classCode);
}

const USB_CLASS_CODE_NAMES = new Map<number, string>([
  [0x00, "Per-interface / composite"],
  [0x01, "Audio"],
  [0x02, "Communications / CDC Control"],
  [0x03, "HID"],
  [0x05, "Physical"],
  [0x06, "Image"],
  [0x07, "Printer"],
  [0x08, "Mass Storage"],
  [0x09, "Hub"],
  [0x0a, "CDC Data"],
  [0x0b, "Smart Card"],
  [0x0d, "Content Security"],
  [0x0e, "Video"],
  [0x0f, "Personal Healthcare"],
  [0x10, "Audio/Video"],
  [0x11, "Billboard"],
  [0x12, "USB Type-C Bridge"],
  [0xdc, "Diagnostic Device"],
  [0xe0, "Wireless Controller"],
  [0xef, "Miscellaneous"],
  [0xfe, "Application Specific"],
  [0xff, "Vendor Specific"],
]);

export function describeUsbClassCode(classCode: number): string {
  const known = USB_CLASS_CODE_NAMES.get(classCode);
  if (known) return known;
  const normalized = classCode & 0xff;
  return `0x${normalized.toString(16).toUpperCase().padStart(2, "0")}`;
}

export function classifyWebUsbDevice(device: USBDevice): WebUsbDeviceClassification {
  const protectedInterfaces: WebUsbInterfaceDescriptor[] = [];
  const unprotectedInterfaces: WebUsbInterfaceDescriptor[] = [];

  // `USBDevice.configurations` provides the set of descriptors available on the
  // device without requiring `open()` / `selectConfiguration()`.
  for (const configuration of device.configurations ?? []) {
    for (const iface of configuration.interfaces ?? []) {
      for (const alternate of iface.alternates ?? []) {
        const entry: WebUsbInterfaceDescriptor = {
          configurationValue: configuration.configurationValue,
          interfaceNumber: iface.interfaceNumber,
          alternateSetting: alternate.alternateSetting,
          classCode: alternate.interfaceClass,
          subclassCode: alternate.interfaceSubclass,
          protocolCode: alternate.interfaceProtocol,
        };

        if (isProtectedInterfaceClass(entry.classCode)) {
          protectedInterfaces.push(entry);
        } else {
          unprotectedInterfaces.push(entry);
        }
      }
    }
  }

  return {
    hasAnyInterfaces: protectedInterfaces.length + unprotectedInterfaces.length > 0,
    hasUnprotectedInterfaces: unprotectedInterfaces.length > 0,
    protected: protectedInterfaces,
    unprotected: unprotectedInterfaces,
  };
}
