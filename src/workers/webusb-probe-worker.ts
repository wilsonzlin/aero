/// <reference lib="webworker" />

const ctx = self as unknown as DedicatedWorkerGlobalScope;

type ProbeRequest =
  | { type: 'probe' }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  | { type: 'device'; device: any };

type ProbeResponse =
  | { type: 'probe-result'; report: unknown }
  | { type: 'device-result'; report: unknown }
  | { type: 'error'; error: { name: string; message: string } };

function serializeError(err: unknown): { name: string; message: string } {
  if (err instanceof DOMException) return { name: err.name, message: err.message };
  if (err instanceof Error) return { name: err.name, message: err.message };
  if (err && typeof err === 'object') {
    const maybe = err as { name?: unknown; message?: unknown };
    const name = typeof maybe.name === 'string' ? maybe.name : 'Error';
    const message = typeof maybe.message === 'string' ? maybe.message : String(err);
    return { name, message };
  }
  return { name: 'Error', message: String(err) };
}

function summarizeUsbDevice(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  device: any,
): Record<string, unknown> | null {
  if (!device || typeof device !== 'object') return null;
  return {
    productName: device.productName,
    manufacturerName: device.manufacturerName,
    serialNumber: device.serialNumber,
    vendorId: device.vendorId,
    productId: device.productId,
    opened: device.opened,
  };
}

async function probe(): Promise<unknown> {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const usb: any = (navigator as unknown as { usb?: unknown }).usb;
  const hasUsb = typeof usb !== 'undefined';

  const report: Record<string, unknown> = {
    isSecureContext: (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true,
    hasUsb,
    hasGetDevices: typeof usb?.getDevices === 'function',
    hasRequestDevice: typeof usb?.requestDevice === 'function',
  };

  if (typeof usb?.getDevices === 'function') {
    try {
      const devices = await usb.getDevices();
      report.getDevices = {
        ok: true,
        count: Array.isArray(devices) ? devices.length : null,
        devices: Array.isArray(devices) ? devices.map(summarizeUsbDevice) : null,
      };
    } catch (err) {
      report.getDevices = {
        ok: false,
        error: err instanceof Error ? err.message : String(err),
      };
    }
  }

  if (typeof usb?.requestDevice === 'function') {
    try {
      // In specs/Chromium, this is expected to reject in workers due to missing transient user activation.
      // Use a dummy filter so the call reaches the user-activation check rather than failing validation.
      const device = await usb.requestDevice({ filters: [{ vendorId: 0 }] });
      report.requestDevice = { ok: true, device: summarizeUsbDevice(device) };
    } catch (err) {
      report.requestDevice = { ok: false, error: serializeError(err) };
    }
  }

  return report;
}

async function probeDevice(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  device: any,
): Promise<unknown> {
  const report: Record<string, unknown> = {
    received: summarizeUsbDevice(device),
    hasOpen: typeof device?.open === 'function',
    hasClose: typeof device?.close === 'function',
  };

  const openedBefore = typeof device?.opened === 'boolean' ? device.opened : null;
  let openOk = false;
  let closeOk = false;
  let openError: unknown = null;
  let closeError: unknown = null;

  try {
    if (typeof device?.open !== 'function') throw new TypeError('device.open is not a function');
    await device.open();
    openOk = true;
  } catch (err) {
    openError = serializeError(err);
  }

  try {
    if (typeof device?.close !== 'function') throw new TypeError('device.close is not a function');
    await device.close();
    closeOk = true;
  } catch (err) {
    closeError = serializeError(err);
  }

  const openedAfter = typeof device?.opened === 'boolean' ? device.opened : null;
  report.openClose = {
    openedBefore,
    openedAfter,
    openOk,
    closeOk,
    ...(openError ? { openError } : {}),
    ...(closeError ? { closeError } : {}),
  };

  return report;
}

ctx.onmessage = (ev: MessageEvent<ProbeRequest>) => {
  const msg = ev.data;

  switch (msg.type) {
    case 'probe': {
      void probe()
        .then((report) => {
          const resp: ProbeResponse = { type: 'probe-result', report };
          ctx.postMessage(resp);
        })
        .catch((err) => {
          const resp: ProbeResponse = { type: 'error', error: serializeError(err) };
          ctx.postMessage(resp);
        });
      break;
    }
    case 'device': {
      void probeDevice(msg.device)
        .then((report) => {
          const resp: ProbeResponse = { type: 'device-result', report };
          ctx.postMessage(resp);
        })
        .catch((err) => {
          const resp: ProbeResponse = { type: 'error', error: serializeError(err) };
          ctx.postMessage(resp);
        });
      break;
    }
    default: {
      const _exhaustive: never = msg;
      const resp: ProbeResponse = {
        type: 'error',
        error: { name: 'Error', message: `Unknown message: ${(msg as { type?: unknown }).type}` },
      };
      ctx.postMessage(resp);
    }
  }
};
