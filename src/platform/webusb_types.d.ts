export {};

// WebUSB type definitions for the repo-root harness.
//
// TypeScript's standard DOM lib does not currently ship WebUSB typings, and the
// repo's CI/typecheck pipeline should not depend on optional `@types/*` installs.
//
// Keep these definitions minimal but spec-compatible so they can merge cleanly
// if/when upstream lib.dom.d.ts (or a separate `@types` package) provides the
// same names.

declare global {
  type USBRequestType = 'standard' | 'class' | 'vendor';
  type USBRecipient = 'device' | 'interface' | 'endpoint' | 'other';
  type USBTransferStatus = 'ok' | 'stall' | 'babble' | 'error';

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
    /**
     * Chromium-only extension used by the harness to probe chooser behavior.
     *
     * When true, the chooser may show all devices (subject to origin security
     * policy) without requiring an explicit filter match.
     */
    acceptAllDevices?: boolean;
  }

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

  interface USBEndpoint {
    endpointNumber: number;
    direction: 'in' | 'out';
    type: 'bulk' | 'interrupt' | 'isochronous';
    packetSize: number;
  }

  interface USBAlternateInterface {
    alternateSetting: number;
    interfaceClass: number;
    interfaceSubclass: number;
    interfaceProtocol: number;
    interfaceName?: string | null;
    endpoints: USBEndpoint[];
  }

  interface USBInterface {
    interfaceNumber: number;
    alternates: USBAlternateInterface[];
    claimed?: boolean;
  }

  interface USBConfiguration {
    configurationValue: number;
    configurationName?: string | null;
    interfaces: USBInterface[];
  }

  interface USBDevice extends EventTarget {
    readonly vendorId: number;
    readonly productId: number;
    readonly productName?: string | null;
    readonly manufacturerName?: string | null;
    readonly serialNumber?: string | null;
    readonly opened: boolean;
    readonly configuration: USBConfiguration | null;
    readonly configurations?: USBConfiguration[];

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
    requestDevice(options?: USBDeviceRequestOptions): Promise<USBDevice>;
    getDevices(): Promise<USBDevice[]>;
    onconnect?: ((this: USB, ev: USBConnectionEvent) => any) | null;
    ondisconnect?: ((this: USB, ev: USBConnectionEvent) => any) | null;

    addEventListener(
      type: 'connect' | 'disconnect',
      listener: ((this: USB, ev: USBConnectionEvent) => any) | null,
      options?: boolean | AddEventListenerOptions,
    ): void;
    removeEventListener(
      type: 'connect' | 'disconnect',
      listener: ((this: USB, ev: USBConnectionEvent) => any) | null,
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
