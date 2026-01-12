// NOTE: Legacy demo-only WebUSB broker/client stack (repo-root Vite harness).
// (Quarantined under `src/platform/legacy/`; not used by the canonical runtime.)
//
// This module implements a generic main-thread WebUSB broker used by the repo-root Vite harness
// (`src/main.ts`) for diagnostics / demos. It forwards *direct* `navigator.usb` operations to a
// worker over a MessagePort.
//
// WARNING: Do NOT extend this stack for production guest USB passthrough. The canonical browser USB
// passthrough stack (UsbHostAction/UsbHostCompletion) lives in `web/src/usb/*` and the wire contract
// is owned by:
// - crates/aero-usb/src/passthrough.rs
// - docs/fixtures/webusb_passthrough_wire.json
// - web/src/usb/usb_passthrough_types.ts
//
// Deletion target per docs/adr/0015-canonical-usb-stack.md.

import {
  WEBUSB_BROKER_PORT_MESSAGE_TYPE,
  type WebUsbBrokerEvent,
  type WebUsbBrokerPortMessage,
  type WebUsbBrokerToClientMessage,
  type WebUsbDeviceId,
  type WebUsbDeviceInfo,
  type WebUsbInTransferResult,
  type WebUsbOutTransferResult,
  type WebUsbRequest,
  type WebUsbResponse,
  getTransferablesForWebUsbResponse,
  serializeWebUsbError,
} from './webusb_protocol';

/** @deprecated Legacy demo-only WebUSB broker (direct `navigator.usb` RPC). Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbBroker = {
  attachToWorker(worker: Worker): void;
  requestDevice(options?: USBDeviceRequestOptions): Promise<WebUsbDeviceInfo>;
  getDevices(): WebUsbDeviceInfo[];
};

function getNavigatorUsb(): USB | undefined {
  if (typeof navigator === 'undefined') return undefined;
  return (navigator as Navigator & { usb?: USB }).usb;
}

function requireNavigatorUsb(): USB {
  const usb = getNavigatorUsb();
  if (!usb) {
    throw new Error('WebUSB is not available in this browser/context (navigator.usb is missing).');
  }
  return usb;
}

function toDeviceInfo(deviceId: WebUsbDeviceId, device: USBDevice): WebUsbDeviceInfo {
  return {
    deviceId,
    vendorId: device.vendorId,
    productId: device.productId,
    productName: device.productName ?? null,
    manufacturerName: device.manufacturerName ?? null,
    serialNumber: device.serialNumber ?? null,
    opened: device.opened,
  };
}

function serializeInTransferResult(result: USBInTransferResult): WebUsbInTransferResult {
  const data = result.data;
  if (!data) return { status: result.status };

  // `DataView.buffer` is typed as `ArrayBufferLike` because it can technically
  // wrap a SharedArrayBuffer. WebUSB transfers should return an ArrayBuffer in
  // practice, but fall back to copying if we ever encounter a SAB.
  const buf = data.buffer;
  if (buf instanceof ArrayBuffer) {
    return {
      status: result.status,
      data: buf,
      dataOffset: data.byteOffset,
      dataLength: data.byteLength,
    };
  }

  const copy = new Uint8Array(data.byteLength);
  copy.set(new Uint8Array(buf, data.byteOffset, data.byteLength));
  return { status: result.status, data: copy.buffer, dataOffset: 0, dataLength: copy.byteLength };
}

function serializeOutTransferResult(result: USBOutTransferResult): WebUsbOutTransferResult {
  return { status: result.status, bytesWritten: result.bytesWritten };
}

function checkUserActivationForRequestDevice(): void {
  if (typeof window === 'undefined' || typeof document === 'undefined') {
    throw new Error('navigator.usb.requestDevice must be called on the main thread.');
  }

  const maybeActivation = (navigator as Navigator & { userActivation?: { isActive: boolean } }).userActivation;
  if (maybeActivation && !maybeActivation.isActive) {
    throw new Error('navigator.usb.requestDevice must be called from a user gesture (userActivation.isActive=false).');
  }
}

/** @deprecated Legacy demo-only WebUSB broker factory. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export function createWebUsbBroker(): WebUsbBroker {
  let nextDeviceId = 1;
  const devices = new Map<WebUsbDeviceId, USBDevice>();
  const disconnectedDeviceIds = new Set<WebUsbDeviceId>();
  const ports = new Set<MessagePort>();

  function getDeviceOrThrow(deviceId: WebUsbDeviceId): USBDevice {
    const device = devices.get(deviceId);
    if (device) return device;
    if (disconnectedDeviceIds.has(deviceId)) {
      throw new Error(`USB device ${deviceId} has been disconnected.`);
    }
    throw new Error(`Unknown USB deviceId: ${deviceId}`);
  }

  function broadcast(event: WebUsbBrokerEvent): void {
    const msg: WebUsbBrokerToClientMessage = { type: 'event', event };
    for (const port of ports) {
      try {
        port.postMessage(msg);
      } catch {
        ports.delete(port);
      }
    }
  }

  const usb = getNavigatorUsb();
  if (usb) {
    usb.addEventListener('disconnect', (event: USBConnectionEvent) => {
      const removed: WebUsbDeviceId[] = [];
      for (const [deviceId, device] of devices) {
        if (device === event.device) removed.push(deviceId);
      }
      for (const deviceId of removed) {
        devices.delete(deviceId);
        disconnectedDeviceIds.add(deviceId);
        broadcast({ type: 'disconnect', deviceId });
      }
    });
  }

  function getDevices(): WebUsbDeviceInfo[] {
    return Array.from(devices.entries()).map(([deviceId, device]) => toDeviceInfo(deviceId, device));
  }

  async function requestDevice(options: USBDeviceRequestOptions = { filters: [] }): Promise<WebUsbDeviceInfo> {
    checkUserActivationForRequestDevice();

    const usb = requireNavigatorUsb();
    const device = await usb.requestDevice(options);
    const deviceId = nextDeviceId++;
    devices.set(deviceId, device);
    disconnectedDeviceIds.delete(deviceId);
    return toDeviceInfo(deviceId, device);
  }

  async function handleRequest(request: WebUsbRequest): Promise<WebUsbResponse> {
    try {
      switch (request.type) {
        case 'requestDevice': {
          const device = await requestDevice(request.options);
          return { id: request.id, ok: true, type: 'requestDevice', deviceId: device.deviceId, device };
        }
        case 'getDevices':
          return { id: request.id, ok: true, type: 'getDevices', devices: getDevices() };
        case 'open': {
          const device = getDeviceOrThrow(request.deviceId);
          await device.open();
          return { id: request.id, ok: true, type: 'open' };
        }
        case 'close': {
          const device = getDeviceOrThrow(request.deviceId);
          await device.close();
          return { id: request.id, ok: true, type: 'close' };
        }
        case 'selectConfiguration': {
          const device = getDeviceOrThrow(request.deviceId);
          await device.selectConfiguration(request.configurationValue);
          return { id: request.id, ok: true, type: 'selectConfiguration' };
        }
        case 'claimInterface': {
          const device = getDeviceOrThrow(request.deviceId);
          await device.claimInterface(request.interfaceNumber);
          return { id: request.id, ok: true, type: 'claimInterface' };
        }
        case 'releaseInterface': {
          const device = getDeviceOrThrow(request.deviceId);
          await device.releaseInterface(request.interfaceNumber);
          return { id: request.id, ok: true, type: 'releaseInterface' };
        }
        case 'reset': {
          const device = getDeviceOrThrow(request.deviceId);
          await device.reset();
          return { id: request.id, ok: true, type: 'reset' };
        }
        case 'controlTransferIn': {
          const device = getDeviceOrThrow(request.deviceId);
          const result = await device.controlTransferIn(request.setup, request.length);
          return { id: request.id, ok: true, type: 'controlTransferIn', result: serializeInTransferResult(result) };
        }
        case 'controlTransferOut': {
          const device = getDeviceOrThrow(request.deviceId);
          const result = await device.controlTransferOut(request.setup, request.data);
          return { id: request.id, ok: true, type: 'controlTransferOut', result: serializeOutTransferResult(result) };
        }
        case 'transferIn': {
          const device = getDeviceOrThrow(request.deviceId);
          const result = await device.transferIn(request.endpointNumber, request.length);
          return { id: request.id, ok: true, type: 'transferIn', result: serializeInTransferResult(result) };
        }
        case 'transferOut': {
          const device = getDeviceOrThrow(request.deviceId);
          const result = await device.transferOut(request.endpointNumber, request.data);
          return { id: request.id, ok: true, type: 'transferOut', result: serializeOutTransferResult(result) };
        }
        default: {
          const neverType: never = request;
          throw new Error(`Unhandled WebUSB request: ${(neverType as WebUsbRequest).type}`);
        }
      }
    } catch (err) {
      return { id: request.id, ok: false, type: request.type, error: serializeWebUsbError(err) };
    }
  }

  function attachToWorker(worker: Worker): void {
    const channel = new MessageChannel();
    const port = channel.port1;
    ports.add(port);

    port.addEventListener('message', (event: MessageEvent<WebUsbRequest>) => {
      void (async () => {
        const response = await handleRequest(event.data);
        port.postMessage(response, getTransferablesForWebUsbResponse(response));
      })();
    });
    port.addEventListener('messageerror', () => {
      ports.delete(port);
    });
    port.start();

    const init: WebUsbBrokerPortMessage = { type: WEBUSB_BROKER_PORT_MESSAGE_TYPE, port: channel.port2 };
    worker.postMessage(init, [channel.port2]);
  }

  return { attachToWorker, requestDevice, getDevices };
}
