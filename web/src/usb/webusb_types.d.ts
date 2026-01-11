// Minimal WebUSB type definitions.
//
// TypeScript's built-in DOM lib does not currently ship WebUSB typings, but the
// runtime objects exist in Chromium (`navigator.usb`, `USBDevice`, etc.). Keep
// this surface intentionally small: only what our WebUSB backend needs.

export {};

declare global {
  type USBRequestType = "standard" | "class" | "vendor";
  type USBRecipient = "device" | "interface" | "endpoint" | "other";
  type USBTransferStatus = "ok" | "stall" | "babble";

  interface USBControlTransferParameters {
    requestType: USBRequestType;
    recipient: USBRecipient;
    request: number;
    value: number;
    index: number;
  }

  interface USBInTransferResult {
    data?: DataView;
    status: USBTransferStatus;
  }

  interface USBOutTransferResult {
    bytesWritten: number;
    status: USBTransferStatus;
  }

  interface USBInterface {
    interfaceNumber: number;
  }

  interface USBConfiguration {
    configurationValue: number;
    interfaces: USBInterface[];
  }

  interface USBDevice {
    readonly opened: boolean;
    readonly configuration: USBConfiguration | null;
    readonly configurations: USBConfiguration[];

    open(): Promise<void>;
    selectConfiguration(configurationValue: number): Promise<void>;
    claimInterface(interfaceNumber: number): Promise<void>;

    controlTransferIn(
      setup: USBControlTransferParameters,
      length: number,
    ): Promise<USBInTransferResult>;
    controlTransferOut(
      setup: USBControlTransferParameters,
      data?: BufferSource,
    ): Promise<USBOutTransferResult>;

    transferIn(endpointNumber: number, length: number): Promise<USBInTransferResult>;
    transferOut(endpointNumber: number, data: BufferSource): Promise<USBOutTransferResult>;
  }
}

