/// <reference lib="webworker" />

import { WEBUSB_BROKER_PORT_MESSAGE_TYPE } from '../platform/legacy/webusb_protocol';
import { installWebUsbClientInWorker } from '../platform/legacy/webusb_client';

type MainToWorkerMessage = { type: 'WebUsbDemoRun'; deviceId: number };

type WorkerToMainMessage =
  | { type: 'WebUsbDemoReady' }
  | { type: 'WebUsbDemoLog'; line: string }
  | { type: 'WebUsbDemoDone'; ok: boolean; error?: string };

const ctx = self as unknown as DedicatedWorkerGlobalScope;

function postMessageToMain(msg: WorkerToMainMessage): void {
  ctx.postMessage(msg);
}

function formatError(err: unknown): string {
  return err instanceof Error ? `${err.name}: ${err.message}` : String(err);
}

const clientPromise = installWebUsbClientInWorker();
clientPromise
  .then((client) => {
    client.onBrokerEvent((event) => {
      postMessageToMain({ type: 'WebUsbDemoLog', line: `broker event: ${event.type} deviceId=${event.deviceId}` });
    });
    postMessageToMain({ type: 'WebUsbDemoReady' });
  })
  .catch((err) => {
    postMessageToMain({ type: 'WebUsbDemoDone', ok: false, error: formatError(err) });
  });

let running = false;

async function runDemo(deviceId: number): Promise<void> {
  if (running) {
    postMessageToMain({ type: 'WebUsbDemoLog', line: 'Demo already running; ignoring duplicate request.' });
    return;
  }
  running = true;

  const client = await clientPromise;

  postMessageToMain({ type: 'WebUsbDemoLog', line: `Opening deviceId=${deviceId}…` });
  try {
    await client.open(deviceId);
    postMessageToMain({ type: 'WebUsbDemoLog', line: 'Device opened.' });

    // Standard USB device descriptor read (should work on most devices).
    // GET_DESCRIPTOR (bRequest=0x06), wValue=0x0100 (DEVICE descriptor), wIndex=0.
    try {
      const result = await client.controlTransferIn(
        deviceId,
        { requestType: 'standard', recipient: 'device', request: 0x06, value: 0x0100, index: 0 },
        18,
      );
      const data = result.data;
      if (!data) {
        postMessageToMain({ type: 'WebUsbDemoLog', line: `controlTransferIn status=${result.status} (no data)` });
      } else {
        const offset = result.dataOffset ?? 0;
        const length = result.dataLength ?? data.byteLength - offset;
        const bytes = new Uint8Array(data, offset, Math.max(0, Math.min(length, data.byteLength - offset)));
        const hex = Array.from(bytes)
          .map((b) => b.toString(16).padStart(2, '0'))
          .join(' ');

        const vid = bytes.length >= 10 ? bytes[8] | (bytes[9] << 8) : null;
        const pid = bytes.length >= 12 ? bytes[10] | (bytes[11] << 8) : null;

        postMessageToMain({
          type: 'WebUsbDemoLog',
          line:
            `controlTransferIn status=${result.status} bytes=${bytes.byteLength}` +
            (vid !== null && pid !== null ? ` vid=0x${vid.toString(16)} pid=0x${pid.toString(16)}` : ''),
        });
        postMessageToMain({ type: 'WebUsbDemoLog', line: `device descriptor: ${hex}` });
      }
    } catch (err) {
      postMessageToMain({ type: 'WebUsbDemoLog', line: `controlTransferIn failed: ${formatError(err)}` });
    }
  } catch (err) {
    postMessageToMain({ type: 'WebUsbDemoDone', ok: false, error: formatError(err) });
    return;
  } finally {
    try {
      await client.close(deviceId);
      postMessageToMain({ type: 'WebUsbDemoLog', line: 'Device closed.' });
    } catch (err) {
      postMessageToMain({ type: 'WebUsbDemoLog', line: `close failed: ${formatError(err)}` });
    }
    running = false;
  }

  postMessageToMain({ type: 'WebUsbDemoDone', ok: true });
}

ctx.addEventListener('message', (event: MessageEvent) => {
  const data = event.data as unknown;
  if (!data || typeof data !== 'object') return;
  const maybeType = (data as { type?: unknown }).type;

  // Ignore the WebUSB broker init handshake message (consumed by installWebUsbClientInWorker).
  if (maybeType === WEBUSB_BROKER_PORT_MESSAGE_TYPE) return;

  const msg = data as Partial<MainToWorkerMessage>;
  if (msg.type === 'WebUsbDemoRun' && typeof msg.deviceId === 'number') {
    void runDemo(msg.deviceId);
  }
});
