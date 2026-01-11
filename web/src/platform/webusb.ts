export interface WebUsbAlternateClassification {
  interfaceNumber: number;
  alternateSetting: number;
  classCode: number;
  subclassCode: number;
  protocolCode: number;
  className: string | null;
  isProtected: boolean;
  reason: string;
}

export interface WebUsbInterfaceClassification {
  interfaceNumber: number;
  alternates: WebUsbAlternateClassification[];
  /**
   * True if this interface exposes at least one alternate setting that is not
   * blocked by the WebUSB protected interface class list.
   */
  isClaimable: boolean;
}

export interface WebUsbConfigurationClassification {
  configurationValue: number;
  interfaces: WebUsbInterfaceClassification[];
  /**
   * True if this configuration contains at least one claimable interface.
   */
  isClaimable: boolean;
}

export interface WebUsbDeviceClassification {
  configurations: WebUsbConfigurationClassification[];
  /**
   * True if the device contains at least one claimable interface across all
   * configurations.
   */
  isClaimable: boolean;
}

const USB_CLASS_NAMES: Record<number, string> = {
  0x00: "Per-interface / composite",
  0x01: "Audio",
  0x02: "Communications / CDC Control",
  0x03: "HID",
  0x05: "Physical",
  0x06: "Image",
  0x07: "Printer",
  0x08: "Mass Storage",
  0x09: "Hub",
  0x0a: "CDC Data",
  0x0b: "Smart Card",
  0x0d: "Content Security",
  0x0e: "Video",
  0x0f: "Personal Healthcare",
  0x10: "Audio/Video",
  0x11: "Billboard",
  0x12: "USB Type-C Bridge",
  0xdc: "Diagnostic Device",
  0xe0: "Wireless Controller",
  0xef: "Miscellaneous",
  0xfe: "Application Specific",
  0xff: "Vendor Specific",
};

// Chromium (and therefore most WebUSB deployments) blocks certain USB interface
// classes from being claimed over WebUSB. The exact list is browser-defined.
// This set is used as a best-effort diagnostic classifier rather than an API
// contract; devices may still fail to open/claim for OS/driver reasons even when
// the interface class is not on this list.
const WEBUSB_PROTECTED_INTERFACE_CLASSES = new Set<number>([
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
  0xe0, // Wireless Controller
]);

export function usbClassName(classCode: number): string | null {
  return USB_CLASS_NAMES[classCode] ?? null;
}

export function isWebUsbProtectedInterfaceClass(classCode: number): boolean {
  return WEBUSB_PROTECTED_INTERFACE_CLASSES.has(classCode);
}

function classifyAlternate(
  interfaceNumber: number,
  alternate: Pick<
    USBAlternateInterface,
    "alternateSetting" | "interfaceClass" | "interfaceSubclass" | "interfaceProtocol"
  >,
): WebUsbAlternateClassification {
  const classCode = alternate.interfaceClass;
  const className = usbClassName(classCode);
  const isProtected = isWebUsbProtectedInterfaceClass(classCode);

  const reason = isProtected
    ? `Protected interface class${className ? ` (${className})` : ""}`
    : "Not in protected interface class list";

  return {
    interfaceNumber,
    alternateSetting: alternate.alternateSetting,
    classCode,
    subclassCode: alternate.interfaceSubclass,
    protocolCode: alternate.interfaceProtocol,
    className,
    isProtected,
    reason,
  };
}

/**
 * Best-effort WebUSB accessibility classifier.
 *
 * Given a `USBDevice`, returns a per-configuration/interface breakdown that
 * marks interfaces as "claimable" if at least one of their alternate settings is
 * not blocked by the protected interface class list.
 */
export function classifyWebUsbDevice(device: USBDevice): WebUsbDeviceClassification {
  const configurations = (device.configurations ?? []).map((cfg) => {
    const interfaces = cfg.interfaces.map((iface) => {
      const alternates = iface.alternates.map((alt) => classifyAlternate(iface.interfaceNumber, alt));
      const isClaimable = alternates.some((alt) => !alt.isProtected);
      return {
        interfaceNumber: iface.interfaceNumber,
        alternates,
        isClaimable,
      };
    });

    const isClaimable = interfaces.some((iface) => iface.isClaimable);
    return { configurationValue: cfg.configurationValue, interfaces, isClaimable };
  });

  return {
    configurations,
    isClaimable: configurations.some((cfg) => cfg.isClaimable),
  };
}

