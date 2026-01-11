/// <reference lib="webworker" />

const ctx = self as unknown as DedicatedWorkerGlobalScope;

type ProbeRequest =
  | { type: 'probe' }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  | { type: 'clone-test'; device: any };

type ProbeResponse =
  | { type: 'probe-result'; report: unknown }
  | { type: 'clone-result'; report: unknown }
  | { type: 'error'; error: string };

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
          const resp: ProbeResponse = { type: 'error', error: err instanceof Error ? err.message : String(err) };
          ctx.postMessage(resp);
        });
      break;
    }
    case 'clone-test': {
      try {
        const report = {
          received: summarizeUsbDevice(msg.device),
          hasOpen: typeof msg.device?.open === 'function',
          hasTransferIn: typeof msg.device?.transferIn === 'function',
          hasTransferOut: typeof msg.device?.transferOut === 'function',
        };
        const resp: ProbeResponse = { type: 'clone-result', report };
        ctx.postMessage(resp);
      } catch (err) {
        const resp: ProbeResponse = { type: 'error', error: err instanceof Error ? err.message : String(err) };
        ctx.postMessage(resp);
      }
      break;
    }
    default: {
      const _exhaustive: never = msg;
      const resp: ProbeResponse = { type: 'error', error: `Unknown message: ${(msg as { type?: unknown }).type}` };
      ctx.postMessage(resp);
    }
  }
};

