export {};

declare global {
  // Minimal WebUSB + WebHID type definitions.
  //
  // In some minimal environments (including this repo's CI harness), the
  // `@types/w3c-web-usb` / `@types/w3c-web-hid` packages may be unavailable.
  // Keep these definitions in-repo so `tsc` can typecheck WebUSB/WebHID code.

  type USBRequestType = "standard" | "class" | "vendor";
  type USBRecipient = "device" | "interface" | "endpoint" | "other";
  type USBTransferStatus = "ok" | "stall" | "babble";
  type USBDirection = "in" | "out";
  type USBEndpointType = "bulk" | "interrupt" | "isochronous";

  interface USBDeviceFilter {
    vendorId?: number;
    productId?: number;
    classCode?: number;
    subclassCode?: number;
    protocolCode?: number;
    serialNumber?: string;
  }

  interface USBDeviceRequestOptions {
    filters: USBDeviceFilter[];
  }

  interface USBControlTransferParameters {
    requestType: USBRequestType;
    recipient: USBRecipient;
    request: number;
    value: number;
    index: number;
  }

  interface USBEndpoint {
    endpointNumber: number;
    direction: USBDirection;
    type: USBEndpointType;
    packetSize: number;
  }

  interface USBAlternateInterface {
    alternateSetting: number;
    interfaceClass: number;
    interfaceSubclass: number;
    interfaceProtocol: number;
    endpoints: USBEndpoint[];
  }

  interface USBInterface {
    interfaceNumber: number;
    alternates: USBAlternateInterface[];
    claimed: boolean;
  }

  interface USBConfiguration {
    configurationValue: number;
    interfaces: USBInterface[];
  }

  interface USBInTransferResult {
    data: DataView | null;
    status: USBTransferStatus;
  }

  interface USBOutTransferResult {
    bytesWritten: number;
    status: USBTransferStatus;
  }

  interface USBDevice extends EventTarget {
    readonly vendorId: number;
    readonly productId: number;
    readonly productName: string | null;
    readonly manufacturerName: string | null;
    readonly serialNumber: string | null;

    opened: boolean;
    configurations: USBConfiguration[];
    configuration: USBConfiguration | null;

    open(): Promise<void>;
    close(): Promise<void>;
    selectConfiguration(configurationValue: number): Promise<void>;
    claimInterface(interfaceNumber: number): Promise<void>;
    releaseInterface(interfaceNumber: number): Promise<void>;
    reset(): Promise<void>;
    controlTransferIn(setup: USBControlTransferParameters, length: number): Promise<USBInTransferResult>;
    controlTransferOut(setup: USBControlTransferParameters, data?: BufferSource): Promise<USBOutTransferResult>;
    transferIn(endpointNumber: number, length: number): Promise<USBInTransferResult>;
    transferOut(endpointNumber: number, data: BufferSource): Promise<USBOutTransferResult>;
  }

  interface USBConnectionEvent extends Event {
    readonly device: USBDevice;
  }

  interface USB extends EventTarget {
    addEventListener(
      type: "connect" | "disconnect",
      listener: (this: USB, ev: USBConnectionEvent) => unknown,
      options?: boolean | AddEventListenerOptions,
    ): void;
    removeEventListener(
      type: "connect" | "disconnect",
      listener: (this: USB, ev: USBConnectionEvent) => unknown,
      options?: boolean | EventListenerOptions,
    ): void;
    getDevices(): Promise<USBDevice[]>;
    requestDevice(options: USBDeviceRequestOptions): Promise<USBDevice>;
  }

  interface Navigator {
    readonly usb?: USB;
  }

  interface HIDDeviceFilter {
    vendorId?: number;
    productId?: number;
    usagePage?: number;
    usage?: number;
  }

  interface HIDDeviceRequestOptions {
    filters: HIDDeviceFilter[];
  }

  type HIDCollectionType =
    | "physical"
    | "application"
    | "logical"
    | "report"
    | "namedArray"
    | "usageSwitch"
    | "usageModifier";

  interface HIDReportItem {
    usagePage: number;
    usages: readonly number[];
    usageMinimum: number;
    usageMaximum: number;
    reportSize: number;
    reportCount: number;
    unitExponent: number;
    unit: number;
    logicalMinimum: number;
    logicalMaximum: number;
    physicalMinimum: number;
    physicalMaximum: number;
    strings: readonly number[];
    stringMinimum: number;
    stringMaximum: number;
    designators: readonly number[];
    designatorMinimum: number;
    designatorMaximum: number;

    isAbsolute: boolean;
    isArray: boolean;
    isBufferedBytes: boolean;
    isConstant: boolean;
    isLinear: boolean;
    isRange: boolean;
    isRelative?: boolean;
    isVolatile: boolean;
    hasNull: boolean;
    hasPreferredState: boolean;
    isWrapped?: boolean;
    wrap?: boolean;
  }

  interface HIDReportInfo {
    reportId: number;
    items: readonly HIDReportItem[];
  }

  interface HIDCollectionInfo {
    usagePage: number;
    usage: number;
    type: HIDCollectionType;
    children: readonly HIDCollectionInfo[];
    inputReports: readonly HIDReportInfo[];
    outputReports: readonly HIDReportInfo[];
    featureReports: readonly HIDReportInfo[];
  }

  interface HIDInputReportEvent extends Event {
    readonly data: DataView;
    readonly device: HIDDevice;
    readonly reportId: number;
  }

  interface HIDDevice extends EventTarget {
    readonly vendorId: number;
    readonly productId: number;
    readonly productName: string;
    readonly collections: readonly HIDCollectionInfo[];
    opened: boolean;
    open(): Promise<void>;
    close(): Promise<void>;

    sendReport(reportId: number, data: BufferSource): Promise<void>;
    sendFeatureReport(reportId: number, data: BufferSource): Promise<void>;

    addEventListener(
      type: "inputreport",
      listener: (this: HIDDevice, ev: HIDInputReportEvent) => unknown,
      options?: boolean | AddEventListenerOptions,
    ): void;
    removeEventListener(
      type: "inputreport",
      listener: (this: HIDDevice, ev: HIDInputReportEvent) => unknown,
      options?: boolean | EventListenerOptions,
    ): void;
  }

  interface HIDConnectionEvent extends Event {
    readonly device: HIDDevice;
  }

  interface HID extends EventTarget {
    addEventListener(
      type: "connect" | "disconnect",
      listener: (this: HID, ev: HIDConnectionEvent) => unknown,
      options?: boolean | AddEventListenerOptions,
    ): void;
    removeEventListener(
      type: "connect" | "disconnect",
      listener: (this: HID, ev: HIDConnectionEvent) => unknown,
      options?: boolean | EventListenerOptions,
    ): void;
    getDevices(): Promise<HIDDevice[]>;
    requestDevice(options: HIDDeviceRequestOptions): Promise<HIDDevice[]>;
  }

  interface Navigator {
    readonly hid?: HID;
  }
}
