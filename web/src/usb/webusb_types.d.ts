export {};

// Minimal WebUSB type definitions.
//
// TypeScript's built-in DOM lib does not currently ship WebUSB typings, but the
// runtime objects exist in Chromium (`navigator.usb`, `USBDevice`, etc.). Keep
// this surface intentionally small: only what our WebUSB smoke test and backend
// need.
//
// Spec: https://wicg.github.io/webusb/
declare global {
  type USBRequestType = "standard" | "class" | "vendor";
  type USBRecipient = "device" | "interface" | "endpoint" | "other";
  type USBTransferStatus = "ok" | "stall" | "babble";

  type USBDirection = "in" | "out";
  type USBEndpointType = "bulk" | "interrupt" | "isochronous";

  interface USBControlTransferParameters {
    requestType: USBRequestType;
    recipient: USBRecipient;
    request: number;
    value: number;
    index: number;
  }

  interface USBInTransferResult {
    data: DataView | null;
    status: USBTransferStatus;
  }

  interface USBOutTransferResult {
    bytesWritten: number;
    status: USBTransferStatus;
  }

  interface USBEndpoint {
    readonly endpointNumber: number;
    readonly direction: USBDirection;
    readonly type: USBEndpointType;
    readonly packetSize: number;
  }

  interface USBAlternateInterface {
    readonly alternateSetting: number;
    readonly interfaceClass: number;
    readonly interfaceSubclass: number;
    readonly interfaceProtocol: number;
    readonly endpoints: USBEndpoint[];
  }

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
    exclusionFilters?: USBDeviceFilter[];
  }

  interface USBInterface {
    readonly interfaceNumber: number;
    readonly claimed: boolean;
    readonly alternates: USBAlternateInterface[];
    readonly alternate: USBAlternateInterface;
  }

  interface USBConfiguration {
    readonly configurationValue: number;
    readonly interfaces: USBInterface[];
  }

  interface USBDevice {
    readonly vendorId: number;
    readonly productId: number;
    readonly productName?: string;
    readonly manufacturerName?: string;
    readonly serialNumber?: string;
    readonly opened: boolean;

    readonly configurations: USBConfiguration[];
    readonly configuration: USBConfiguration | null;

    open(): Promise<void>;
    close?(): Promise<void>;
    selectConfiguration(configurationValue: number): Promise<void>;
    claimInterface(interfaceNumber: number): Promise<void>;
    releaseInterface?(interfaceNumber: number): Promise<void>;

    controlTransferIn(setup: USBControlTransferParameters, length: number): Promise<USBInTransferResult>;
    controlTransferOut(setup: USBControlTransferParameters, data?: BufferSource): Promise<USBOutTransferResult>;

    transferIn(endpointNumber: number, length: number): Promise<USBInTransferResult>;
    transferOut(endpointNumber: number, data: BufferSource): Promise<USBOutTransferResult>;
  }

  interface USB {
    requestDevice(options: USBDeviceRequestOptions): Promise<USBDevice>;
    getDevices?(): Promise<USBDevice[]>;
  }

  interface Navigator {
    readonly usb?: USB;
  }

  interface WorkerNavigator {
    readonly usb?: USB;
  }
}
