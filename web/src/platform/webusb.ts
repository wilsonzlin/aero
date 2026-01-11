import { describeUsbClassCode, isProtectedInterfaceClass } from "./webusb_protection";

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

export function usbClassName(classCode: number): string | null {
  const description = describeUsbClassCode(classCode);
  return description.startsWith("0x") ? null : description;
}

export function isWebUsbProtectedInterfaceClass(classCode: number): boolean {
  return isProtectedInterfaceClass(classCode);
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
