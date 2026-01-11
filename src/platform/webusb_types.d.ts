export {};

// Minimal WebUSB type declarations used by the repo-root harness and helper
// modules under `src/platform/`.
//
// TypeScript's standard `lib.dom.d.ts` historically did not include WebUSB
// (it originated from WICG); this keeps the project typechecking without
// pulling in a full external typings package.
//
// Keep these narrow and mostly-optional so they can merge cleanly if/when the
// upstream DOM lib adds official WebUSB types.

declare global {
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
    acceptAllDevices?: boolean;
  }

  type USBTransferStatus = 'ok' | 'stall' | 'babble';

  interface USBInTransferResult {
    readonly data: DataView | null;
    readonly status: USBTransferStatus;
  }

  interface USBOutTransferResult {
    readonly bytesWritten: number;
    readonly status: USBTransferStatus;
  }

  interface USBControlTransferParameters {
    requestType: 'standard' | 'class' | 'vendor';
    recipient: 'device' | 'interface' | 'endpoint' | 'other';
    request: number;
    value: number;
    index: number;
  }

  interface USBAlternateInterface {
    readonly interfaceClass: number;
    readonly interfaceSubclass: number;
    readonly interfaceProtocol: number;
  }

  interface USBInterface {
    readonly interfaceNumber: number;
    readonly alternates: USBAlternateInterface[];
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
    readonly configuration?: USBConfiguration | null;
    readonly configurations?: USBConfiguration[];

    // Chromium exposes this (non-standard) identifier; the harness uses it for
    // demo wiring, but code should not rely on it always existing.
    readonly deviceId?: string;

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

  interface USB {
    getDevices(): Promise<USBDevice[]>;
    requestDevice(options?: USBDeviceRequestOptions): Promise<USBDevice>;
    addEventListener(
      type: 'connect' | 'disconnect',
      listener: (this: USB, ev: USBConnectionEvent) => unknown,
      options?: boolean | AddEventListenerOptions,
    ): void;
    removeEventListener(
      type: 'connect' | 'disconnect',
      listener: (this: USB, ev: USBConnectionEvent) => unknown,
      options?: boolean | EventListenerOptions,
    ): void;
  }

  interface Navigator {
    usb?: USB;
  }

  interface WorkerNavigator {
    usb?: USB;
  }
}

