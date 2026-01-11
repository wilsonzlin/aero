/// <reference lib="webworker" />

import { formatWebUsbError } from "../platform/webusb_troubleshooting";

type ProbeRequest =
  | { type: "probe" }
  | { type: "clone-test"; device: unknown };

type ProbeResponse =
  | { type: "probe-result"; report: unknown }
  | { type: "clone-result"; report: unknown }
  | { type: "error"; error: string };

function summarizeUsbDevice(
  device: unknown,
): Record<string, unknown> | null {
  if (!device || typeof device !== "object") return null;
  const d = device as {
    productName?: unknown;
    manufacturerName?: unknown;
    serialNumber?: unknown;
    vendorId?: unknown;
    productId?: unknown;
    opened?: unknown;
  };
  return {
    productName: d.productName,
    manufacturerName: d.manufacturerName,
    serialNumber: d.serialNumber,
    vendorId: d.vendorId,
    productId: d.productId,
    opened: d.opened,
  };
}

async function probe(): Promise<unknown> {
  const usb = (navigator as unknown as { usb?: unknown }).usb;
  const hasUsb = typeof usb !== "undefined";
  const usbObj = usb && typeof usb === "object" ? (usb as { getDevices?: unknown; requestDevice?: unknown }) : null;

  const report: Record<string, unknown> = {
    isSecureContext: (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true,
    hasUsb,
    hasGetDevices: typeof usbObj?.getDevices === "function",
    hasRequestDevice: typeof usbObj?.requestDevice === "function",
  };

  if (typeof usbObj?.getDevices === "function") {
    try {
      const devices = await (usb as USB).getDevices();
      report.getDevices = {
        ok: true,
        count: Array.isArray(devices) ? devices.length : null,
        devices: Array.isArray(devices) ? devices.map(summarizeUsbDevice) : null,
      };
    } catch (err) {
      report.getDevices = {
        ok: false,
        error: formatWebUsbError(err),
      };
    }
  }

  return report;
}

const ctx = self as unknown as DedicatedWorkerGlobalScope;

ctx.onmessage = (ev: MessageEvent<ProbeRequest>) => {
  const msg = ev.data;

  switch (msg.type) {
    case "probe": {
      void probe()
        .then((report) => {
          const resp: ProbeResponse = { type: "probe-result", report };
          ctx.postMessage(resp);
        })
        .catch((err) => {
          const resp: ProbeResponse = { type: "error", error: formatWebUsbError(err) };
          ctx.postMessage(resp);
        });
      break;
    }
    case "clone-test": {
      try {
        const deviceObj =
          msg.device && typeof msg.device === "object"
            ? (msg.device as { open?: unknown; transferIn?: unknown; transferOut?: unknown })
            : null;
        const report = {
          received: summarizeUsbDevice(msg.device),
          hasOpen: typeof deviceObj?.open === "function",
          hasTransferIn: typeof deviceObj?.transferIn === "function",
          hasTransferOut: typeof deviceObj?.transferOut === "function",
        };
        const resp: ProbeResponse = { type: "clone-result", report };
        ctx.postMessage(resp);
      } catch (err) {
        const resp: ProbeResponse = { type: "error", error: formatWebUsbError(err) };
        ctx.postMessage(resp);
      }
      break;
    }
    default: {
      const _exhaustive: never = msg;
      const resp: ProbeResponse = { type: "error", error: `Unknown message: ${(msg as { type?: unknown }).type}` };
      ctx.postMessage(resp);
      break;
    }
  }
};
