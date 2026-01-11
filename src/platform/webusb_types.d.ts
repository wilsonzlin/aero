export {};

// TypeScript's standard `lib.dom.d.ts` does not currently ship WebUSB type
// definitions. Keep these declarations minimal and focused on the subset of the
// API used by the WebUSB broker/client abstraction.
//
// Ref: https://wicg.github.io/webusb/
declare global {
  type USBTransferStatus = 'ok' | 'stall' | 'babble';

  type USBRequestType = 'standard' | 'class' | 'vendor';
  type USBRecipient = 'device' | 'interface' | 'endpoint' | 'other';

  interface USBControlTransferParameters {
    requestType: USBRequestType;
    recipient: USBRecipient;
    request: number;
    value: number;
    index: number;
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

  interface USBInTransferResult {
    data: DataView | null;
    status: USBTransferStatus;
  }

  interface USBOutTransferResult {
    bytesWritten: number;
    status: USBTransferStatus;
  }

  interface USBDevice {
    vendorId: number;
    productId: number;
    productName?: string;
    manufacturerName?: string;
    serialNumber?: string;
    opened: boolean;

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
    requestDevice(options: USBDeviceRequestOptions): Promise<USBDevice>;
    getDevices(): Promise<USBDevice[]>;
    addEventListener(
      type: 'connect',
      listener: (this: USB, ev: USBConnectionEvent) => unknown,
      options?: boolean | AddEventListenerOptions,
    ): void;
    addEventListener(
      type: 'disconnect',
      listener: (this: USB, ev: USBConnectionEvent) => unknown,
      options?: boolean | AddEventListenerOptions,
    ): void;
    addEventListener(type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions): void;
  }

  interface Navigator {
    usb?: USB;
  }
}

