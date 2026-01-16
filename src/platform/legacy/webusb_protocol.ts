/**
 * @deprecated Legacy demo-only WebUSB demo RPC protocol (repo-root Vite harness).
 *
 * Do not depend on this for production guest USB passthrough. The canonical browser USB passthrough
 * stack lives under `web/src/usb/*` (ADR 0015).
 */
// NOTE: Legacy demo-only WebUSB broker/client stack (repo-root Vite harness).
// (Quarantined under `src/platform/legacy/`; not used by the canonical runtime.)
//
// This file defines the message schema for the legacy repo-root WebUSB demo RPC:
// `src/platform/legacy/webusb_{broker,client}.ts`. It models *direct* `navigator.usb` operations and is
// intentionally separate from Aero's canonical guest USB passthrough wire contract.
//
// WARNING: Do NOT extend this demo protocol for production USB passthrough into the guest. The
// canonical WebUSB passthrough wire contract (UsbHostAction/UsbHostCompletion) is owned by:
// - crates/aero-usb/src/passthrough.rs
// - docs/fixtures/webusb_passthrough_wire.json
// - web/src/usb/usb_passthrough_types.ts
//
// Deletion target per docs/adr/0015-canonical-usb-stack.md.

import { formatOneLineUtf8, truncateUtf8 } from '../../text.js';

const MAX_ERROR_NAME_BYTES = 128;
const MAX_ERROR_MESSAGE_BYTES = 512;
const MAX_ERROR_STACK_BYTES = 8 * 1024;

/** @deprecated Legacy demo-only WebUSB demo RPC constant. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export const WEBUSB_BROKER_PORT_MESSAGE_TYPE = 'WebUsbBrokerPort' as const;

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbDeviceId = number;

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbDeviceInfo = {
  deviceId: WebUsbDeviceId;
  vendorId: number;
  productId: number;
  productName: string | null;
  manufacturerName: string | null;
  serialNumber: string | null;
  opened: boolean;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbInTransferResult = {
  status: USBTransferStatus;
  data?: ArrayBuffer;
  dataOffset?: number;
  dataLength?: number;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbOutTransferResult = {
  status: USBTransferStatus;
  bytesWritten: number;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbRequestDeviceRequest = {
  id: number;
  type: 'requestDevice';
  options: USBDeviceRequestOptions;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbGetDevicesRequest = {
  id: number;
  type: 'getDevices';
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbOpenRequest = {
  id: number;
  type: 'open';
  deviceId: WebUsbDeviceId;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbCloseRequest = {
  id: number;
  type: 'close';
  deviceId: WebUsbDeviceId;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbSelectConfigurationRequest = {
  id: number;
  type: 'selectConfiguration';
  deviceId: WebUsbDeviceId;
  configurationValue: number;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbClaimInterfaceRequest = {
  id: number;
  type: 'claimInterface';
  deviceId: WebUsbDeviceId;
  interfaceNumber: number;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbReleaseInterfaceRequest = {
  id: number;
  type: 'releaseInterface';
  deviceId: WebUsbDeviceId;
  interfaceNumber: number;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbResetRequest = {
  id: number;
  type: 'reset';
  deviceId: WebUsbDeviceId;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbControlTransferInRequest = {
  id: number;
  type: 'controlTransferIn';
  deviceId: WebUsbDeviceId;
  setup: USBControlTransferParameters;
  length: number;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbControlTransferOutRequest = {
  id: number;
  type: 'controlTransferOut';
  deviceId: WebUsbDeviceId;
  setup: USBControlTransferParameters;
  data?: ArrayBuffer;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbTransferInRequest = {
  id: number;
  type: 'transferIn';
  deviceId: WebUsbDeviceId;
  endpointNumber: number;
  length: number;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbTransferOutRequest = {
  id: number;
  type: 'transferOut';
  deviceId: WebUsbDeviceId;
  endpointNumber: number;
  data: ArrayBuffer;
};

/** @deprecated Legacy demo-only WebUSB demo RPC type union. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbRequest =
  | WebUsbRequestDeviceRequest
  | WebUsbGetDevicesRequest
  | WebUsbOpenRequest
  | WebUsbCloseRequest
  | WebUsbSelectConfigurationRequest
  | WebUsbClaimInterfaceRequest
  | WebUsbReleaseInterfaceRequest
  | WebUsbResetRequest
  | WebUsbControlTransferInRequest
  | WebUsbControlTransferOutRequest
  | WebUsbTransferInRequest
  | WebUsbTransferOutRequest;

/** @deprecated Legacy demo-only WebUSB demo RPC request discriminator type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbRequestType = WebUsbRequest['type'];

/** @deprecated Legacy demo-only WebUSB demo RPC helper type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbRequestByType<T extends WebUsbRequestType> = Extract<WebUsbRequest, { type: T }>;

/** @deprecated Legacy demo-only WebUSB demo RPC error serialization type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbSerializedError = {
  name?: string;
  message: string;
  stack?: string;
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbErrorResponse = {
  id: number;
  ok: false;
  type: WebUsbRequestType;
  error: WebUsbSerializedError;
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbRequestDeviceResponse = {
  id: number;
  ok: true;
  type: 'requestDevice';
  deviceId: WebUsbDeviceId;
  device: WebUsbDeviceInfo;
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbGetDevicesResponse = {
  id: number;
  ok: true;
  type: 'getDevices';
  devices: WebUsbDeviceInfo[];
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbOpenResponse = {
  id: number;
  ok: true;
  type: 'open';
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbCloseResponse = {
  id: number;
  ok: true;
  type: 'close';
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbSelectConfigurationResponse = {
  id: number;
  ok: true;
  type: 'selectConfiguration';
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbClaimInterfaceResponse = {
  id: number;
  ok: true;
  type: 'claimInterface';
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbReleaseInterfaceResponse = {
  id: number;
  ok: true;
  type: 'releaseInterface';
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbResetResponse = {
  id: number;
  ok: true;
  type: 'reset';
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbControlTransferInResponse = {
  id: number;
  ok: true;
  type: 'controlTransferIn';
  result: WebUsbInTransferResult;
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbControlTransferOutResponse = {
  id: number;
  ok: true;
  type: 'controlTransferOut';
  result: WebUsbOutTransferResult;
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbTransferInResponse = {
  id: number;
  ok: true;
  type: 'transferIn';
  result: WebUsbInTransferResult;
};

/** @deprecated Legacy demo-only WebUSB demo RPC response type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbTransferOutResponse = {
  id: number;
  ok: true;
  type: 'transferOut';
  result: WebUsbOutTransferResult;
};

/** @deprecated Legacy demo-only WebUSB demo RPC response union. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbResponse =
  | WebUsbErrorResponse
  | WebUsbRequestDeviceResponse
  | WebUsbGetDevicesResponse
  | WebUsbOpenResponse
  | WebUsbCloseResponse
  | WebUsbSelectConfigurationResponse
  | WebUsbClaimInterfaceResponse
  | WebUsbReleaseInterfaceResponse
  | WebUsbResetResponse
  | WebUsbControlTransferInResponse
  | WebUsbControlTransferOutResponse
  | WebUsbTransferInResponse
  | WebUsbTransferOutResponse;

/** @deprecated Legacy demo-only WebUSB demo RPC helper type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbResponseByType<T extends WebUsbRequestType> = Extract<WebUsbResponse, { type: T }>;

/** @deprecated Legacy demo-only WebUSB demo RPC helper type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbOkResponseByType<T extends WebUsbRequestType> = Extract<WebUsbResponseByType<T>, { ok: true }>;

/** @deprecated Legacy demo-only WebUSB demo RPC broker event type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbBrokerEvent = { type: 'disconnect'; deviceId: WebUsbDeviceId };

/** @deprecated Legacy demo-only WebUSB demo RPC event message type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbEventMessage = { type: 'event'; event: WebUsbBrokerEvent };

/** @deprecated Legacy demo-only WebUSB demo RPC broker->client message union. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbBrokerToClientMessage = WebUsbResponse | WebUsbEventMessage;

/** @deprecated Legacy demo-only WebUSB demo RPC init/handshake message type. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export type WebUsbBrokerPortMessage = {
  type: typeof WEBUSB_BROKER_PORT_MESSAGE_TYPE;
  port: MessagePort;
};

/** @deprecated Legacy demo-only WebUSB demo RPC error serialization helper. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export function serializeWebUsbError(err: unknown): WebUsbSerializedError {
  if (err instanceof Error) {
    const name = formatOneLineUtf8(err.name, MAX_ERROR_NAME_BYTES) || 'Error';
    const message = formatOneLineUtf8(err.message, MAX_ERROR_MESSAGE_BYTES) || 'Error';
    const stack = typeof err.stack === 'string' ? truncateUtf8(err.stack, MAX_ERROR_STACK_BYTES) : undefined;
    return {
      name,
      message,
      stack,
    };
  }

  // DOMException is *not* guaranteed to be an `Error` instance in every runtime,
  // but it usually carries `name` + `message` fields that are important for
  // troubleshooting (NotAllowedError/NetworkError/etc).
  if (err && typeof err === 'object') {
    const maybe = err as { name?: unknown; message?: unknown; stack?: unknown };
    const name = typeof maybe.name === 'string' ? maybe.name : undefined;
    const message = typeof maybe.message === 'string' ? maybe.message : String(err);
    const stack = typeof maybe.stack === 'string' ? maybe.stack : undefined;
    const safeName = name ? formatOneLineUtf8(name, MAX_ERROR_NAME_BYTES) : undefined;
    const safeMessage = formatOneLineUtf8(message, MAX_ERROR_MESSAGE_BYTES) || 'Error';
    const safeStack = stack ? truncateUtf8(stack, MAX_ERROR_STACK_BYTES) : undefined;
    return { name: safeName, message: safeMessage, ...(safeStack ? { stack: safeStack } : {}) };
  }

  const safeMessage = formatOneLineUtf8(String(err), MAX_ERROR_MESSAGE_BYTES) || 'Error';
  return { message: safeMessage };
}

/** @deprecated Legacy demo-only WebUSB demo RPC error deserialization helper. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export function deserializeWebUsbError(err: WebUsbSerializedError): Error {
  const out = new Error(err.message);
  if (err.name) out.name = err.name;
  if (err.stack) out.stack = err.stack;
  return out;
}

/** @deprecated Legacy demo-only WebUSB demo RPC transferables helper. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export function getTransferablesForWebUsbRequest(request: WebUsbRequest): Transferable[] {
  switch (request.type) {
    case 'transferOut':
      return [request.data];
    case 'controlTransferOut':
      return request.data ? [request.data] : [];
    default:
      return [];
  }
}

/** @deprecated Legacy demo-only WebUSB demo RPC transferables helper. Do not use for production guest USB passthrough; use `web/src/usb/*` + `crates/aero-usb` (ADR 0015). */
export function getTransferablesForWebUsbResponse(response: WebUsbResponse): Transferable[] {
  if (!response.ok) return [];
  switch (response.type) {
    case 'controlTransferIn':
    case 'transferIn':
      return response.result.data ? [response.result.data] : [];
    default:
      return [];
  }
}
