// NOTE: Legacy demo-only WebUSB broker/client stack (repo-root Vite harness).
// (Quarantined under `src/platform/legacy/`; not used by the canonical runtime.)
//
// This module is the worker-side client for the generic WebUSB broker used by the repo-root Vite
// harness (`src/main.ts`). It forwards *direct* `navigator.usb` operations over a MessagePort.
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
  type WebUsbRequest,
  type WebUsbResponseByType,
  type WebUsbOkResponseByType,
  deserializeWebUsbError,
  getTransferablesForWebUsbRequest,
} from './webusb_protocol';

type PendingRequest = {
  resolve: (value: WebUsbBrokerToClientMessage) => void;
  reject: (reason: Error) => void;
};

/** @deprecated Legacy demo-only WebUSB client (direct `navigator.usb` RPC). Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export class WebUsbClient {
  private port: MessagePort;
  private nextId = 1;
  private pending = new Map<number, PendingRequest>();
  private eventListeners = new Set<(event: WebUsbBrokerEvent) => void>();

  constructor(port: MessagePort) {
    this.port = port;
    this.port.addEventListener('message', (event: MessageEvent<WebUsbBrokerToClientMessage>) => {
      this.handleMessage(event.data);
    });
    this.port.addEventListener('messageerror', () => {
      this.failAllPending(new Error('WebUSB broker message deserialization failed.'));
    });
    this.port.start();
  }

  onBrokerEvent(listener: (event: WebUsbBrokerEvent) => void): () => void {
    this.eventListeners.add(listener);
    return () => {
      this.eventListeners.delete(listener);
    };
  }

  private failAllPending(err: Error): void {
    for (const pending of this.pending.values()) pending.reject(err);
    this.pending.clear();
  }

  private handleMessage(message: WebUsbBrokerToClientMessage): void {
    if (message.type === 'event') {
      for (const listener of this.eventListeners) {
        try {
          listener(message.event);
        } catch {
          // Ignore client-side listener errors so they don't break RPC.
        }
      }
      return;
    }

    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    pending.resolve(message);
  }

  private callRaw<T extends Omit<WebUsbRequest, 'id'>>(request: T): Promise<WebUsbResponseByType<T['type']>> {
    const id = this.nextId++;
    const msg = { ...request, id } as WebUsbRequest;
    const transfer = getTransferablesForWebUsbRequest(msg);
    return new Promise((resolve, reject) => {
      const pending: PendingRequest = {
        resolve: (response) => resolve(response as WebUsbResponseByType<T['type']>),
        reject,
      };
      this.pending.set(id, pending);
      try {
        this.port.postMessage(msg, transfer);
      } catch (err) {
        this.pending.delete(id);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  private async callOk<T extends Omit<WebUsbRequest, 'id'>>(request: T): Promise<WebUsbOkResponseByType<T['type']>> {
    const response = await this.callRaw(request);
    if (response.ok) return response as WebUsbOkResponseByType<T['type']>;
    throw deserializeWebUsbError(response.error);
  }

  async requestDevice(options: USBDeviceRequestOptions): Promise<WebUsbDeviceInfo> {
    const response = await this.callOk({ type: 'requestDevice', options });
    return response.device;
  }

  async getDevices(): Promise<WebUsbDeviceInfo[]> {
    const response = await this.callOk({ type: 'getDevices' });
    return response.devices;
  }

  async open(deviceId: WebUsbDeviceId): Promise<void> {
    await this.callOk({ type: 'open', deviceId });
  }

  async close(deviceId: WebUsbDeviceId): Promise<void> {
    await this.callOk({ type: 'close', deviceId });
  }

  async selectConfiguration(deviceId: WebUsbDeviceId, configurationValue: number): Promise<void> {
    await this.callOk({ type: 'selectConfiguration', deviceId, configurationValue });
  }

  async claimInterface(deviceId: WebUsbDeviceId, interfaceNumber: number): Promise<void> {
    await this.callOk({ type: 'claimInterface', deviceId, interfaceNumber });
  }

  async releaseInterface(deviceId: WebUsbDeviceId, interfaceNumber: number): Promise<void> {
    await this.callOk({ type: 'releaseInterface', deviceId, interfaceNumber });
  }

  async reset(deviceId: WebUsbDeviceId): Promise<void> {
    await this.callOk({ type: 'reset', deviceId });
  }

  async controlTransferIn(
    deviceId: WebUsbDeviceId,
    setup: USBControlTransferParameters,
    length: number,
  ): Promise<WebUsbInTransferResult> {
    const response = await this.callOk({ type: 'controlTransferIn', deviceId, setup, length });
    return response.result;
  }

  async controlTransferOut(
    deviceId: WebUsbDeviceId,
    setup: USBControlTransferParameters,
    data?: ArrayBuffer,
  ): Promise<number> {
    const response = await this.callOk({ type: 'controlTransferOut', deviceId, setup, data });
    return response.result.bytesWritten;
  }

  async transferIn(deviceId: WebUsbDeviceId, endpointNumber: number, length: number): Promise<WebUsbInTransferResult> {
    const response = await this.callOk({ type: 'transferIn', deviceId, endpointNumber, length });
    return response.result;
  }

  async transferOut(deviceId: WebUsbDeviceId, endpointNumber: number, data: ArrayBuffer): Promise<number> {
    const response = await this.callOk({ type: 'transferOut', deviceId, endpointNumber, data });
    return response.result.bytesWritten;
  }
}

/** @deprecated Legacy demo-only worker bootstrap for the WebUSB demo client. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export function installWebUsbClientInWorker(): Promise<WebUsbClient> {
  const ctx = self as unknown as DedicatedWorkerGlobalScope;
  return new Promise((resolve) => {
    const onMessage = (event: MessageEvent) => {
      const data = event.data as unknown;
      if (!data || typeof data !== 'object') return;
      const maybe = data as Partial<WebUsbBrokerPortMessage>;
      if (maybe.type !== WEBUSB_BROKER_PORT_MESSAGE_TYPE) return;
      if (!maybe.port) return;

      ctx.removeEventListener('message', onMessage);
      resolve(new WebUsbClient(maybe.port));
    };
    ctx.addEventListener('message', onMessage);
  });
}
