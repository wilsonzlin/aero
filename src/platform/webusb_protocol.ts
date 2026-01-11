export const WEBUSB_BROKER_PORT_MESSAGE_TYPE = 'WebUsbBrokerPort' as const;

export type WebUsbDeviceId = number;

export type WebUsbDeviceInfo = {
  deviceId: WebUsbDeviceId;
  vendorId: number;
  productId: number;
  productName: string | null;
  manufacturerName: string | null;
  serialNumber: string | null;
  opened: boolean;
};

export type WebUsbInTransferResult = {
  status: USBTransferStatus;
  data?: ArrayBuffer;
  dataOffset?: number;
  dataLength?: number;
};

export type WebUsbOutTransferResult = {
  status: USBTransferStatus;
  bytesWritten: number;
};

export type WebUsbRequestDeviceRequest = {
  id: number;
  type: 'requestDevice';
  options: USBDeviceRequestOptions;
};

export type WebUsbGetDevicesRequest = {
  id: number;
  type: 'getDevices';
};

export type WebUsbOpenRequest = {
  id: number;
  type: 'open';
  deviceId: WebUsbDeviceId;
};

export type WebUsbCloseRequest = {
  id: number;
  type: 'close';
  deviceId: WebUsbDeviceId;
};

export type WebUsbSelectConfigurationRequest = {
  id: number;
  type: 'selectConfiguration';
  deviceId: WebUsbDeviceId;
  configurationValue: number;
};

export type WebUsbClaimInterfaceRequest = {
  id: number;
  type: 'claimInterface';
  deviceId: WebUsbDeviceId;
  interfaceNumber: number;
};

export type WebUsbReleaseInterfaceRequest = {
  id: number;
  type: 'releaseInterface';
  deviceId: WebUsbDeviceId;
  interfaceNumber: number;
};

export type WebUsbResetRequest = {
  id: number;
  type: 'reset';
  deviceId: WebUsbDeviceId;
};

export type WebUsbControlTransferInRequest = {
  id: number;
  type: 'controlTransferIn';
  deviceId: WebUsbDeviceId;
  setup: USBControlTransferParameters;
  length: number;
};

export type WebUsbControlTransferOutRequest = {
  id: number;
  type: 'controlTransferOut';
  deviceId: WebUsbDeviceId;
  setup: USBControlTransferParameters;
  data?: ArrayBuffer;
};

export type WebUsbTransferInRequest = {
  id: number;
  type: 'transferIn';
  deviceId: WebUsbDeviceId;
  endpointNumber: number;
  length: number;
};

export type WebUsbTransferOutRequest = {
  id: number;
  type: 'transferOut';
  deviceId: WebUsbDeviceId;
  endpointNumber: number;
  data: ArrayBuffer;
};

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

export type WebUsbRequestType = WebUsbRequest['type'];

export type WebUsbRequestByType<T extends WebUsbRequestType> = Extract<WebUsbRequest, { type: T }>;

export type WebUsbSerializedError = {
  name?: string;
  message: string;
  stack?: string;
};

export type WebUsbErrorResponse = {
  id: number;
  ok: false;
  type: WebUsbRequestType;
  error: WebUsbSerializedError;
};

export type WebUsbRequestDeviceResponse = {
  id: number;
  ok: true;
  type: 'requestDevice';
  deviceId: WebUsbDeviceId;
  device: WebUsbDeviceInfo;
};

export type WebUsbGetDevicesResponse = {
  id: number;
  ok: true;
  type: 'getDevices';
  devices: WebUsbDeviceInfo[];
};

export type WebUsbOpenResponse = {
  id: number;
  ok: true;
  type: 'open';
};

export type WebUsbCloseResponse = {
  id: number;
  ok: true;
  type: 'close';
};

export type WebUsbSelectConfigurationResponse = {
  id: number;
  ok: true;
  type: 'selectConfiguration';
};

export type WebUsbClaimInterfaceResponse = {
  id: number;
  ok: true;
  type: 'claimInterface';
};

export type WebUsbReleaseInterfaceResponse = {
  id: number;
  ok: true;
  type: 'releaseInterface';
};

export type WebUsbResetResponse = {
  id: number;
  ok: true;
  type: 'reset';
};

export type WebUsbControlTransferInResponse = {
  id: number;
  ok: true;
  type: 'controlTransferIn';
  result: WebUsbInTransferResult;
};

export type WebUsbControlTransferOutResponse = {
  id: number;
  ok: true;
  type: 'controlTransferOut';
  result: WebUsbOutTransferResult;
};

export type WebUsbTransferInResponse = {
  id: number;
  ok: true;
  type: 'transferIn';
  result: WebUsbInTransferResult;
};

export type WebUsbTransferOutResponse = {
  id: number;
  ok: true;
  type: 'transferOut';
  result: WebUsbOutTransferResult;
};

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

export type WebUsbResponseByType<T extends WebUsbRequestType> = Extract<WebUsbResponse, { type: T }>;

export type WebUsbOkResponseByType<T extends WebUsbRequestType> = Extract<WebUsbResponseByType<T>, { ok: true }>;

export type WebUsbBrokerEvent = { type: 'disconnect'; deviceId: WebUsbDeviceId };

export type WebUsbEventMessage = { type: 'event'; event: WebUsbBrokerEvent };

export type WebUsbBrokerToClientMessage = WebUsbResponse | WebUsbEventMessage;

export type WebUsbBrokerPortMessage = {
  type: typeof WEBUSB_BROKER_PORT_MESSAGE_TYPE;
  port: MessagePort;
};

export function serializeWebUsbError(err: unknown): WebUsbSerializedError {
  if (err instanceof Error) {
    return {
      name: err.name,
      message: err.message,
      stack: typeof err.stack === 'string' ? err.stack : undefined,
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
    return { name, message, stack };
  }

  return { message: String(err) };
}

export function deserializeWebUsbError(err: WebUsbSerializedError): Error {
  const out = new Error(err.message);
  if (err.name) out.name = err.name;
  if (err.stack) out.stack = err.stack;
  return out;
}

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
