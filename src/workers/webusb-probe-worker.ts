/// <reference lib="webworker" />

import { formatOneLineUtf8 } from '../text.js';

const ctx = self as unknown as DedicatedWorkerGlobalScope;

type ProbeRequest =
  | { id: number; type: 'probe' }
  | {
      id: number;
      type: 'match_and_open';
      criteria: { vendorId: number; productId: number; serialNumber?: string };
    }
  | { id: number; type: 'device'; device: unknown };

type ProbeResponse =
  | { id: number; type: 'probe-result'; report: unknown }
  | { id: number; type: 'match_and_open_result'; report: unknown }
  | { id: number; type: 'device-result'; report: unknown }
  | { id: number; type: 'error'; error: { name: string; message: string } };

const MAX_ERROR_NAME_BYTES = 128;
const MAX_ERROR_MESSAGE_BYTES = 512;

function serializeError(err: unknown): { name: string; message: string } {
  if (err instanceof DOMException) {
    const name = formatOneLineUtf8(err.name, MAX_ERROR_NAME_BYTES) || 'Error';
    const message = formatOneLineUtf8(err.message, MAX_ERROR_MESSAGE_BYTES) || 'Error';
    return { name, message };
  }
  if (err instanceof Error) {
    const name = formatOneLineUtf8(err.name, MAX_ERROR_NAME_BYTES) || 'Error';
    const message = formatOneLineUtf8(err.message, MAX_ERROR_MESSAGE_BYTES) || 'Error';
    return { name, message };
  }
  if (err && typeof err === 'object') {
    const maybe = err as { name?: unknown; message?: unknown };
    const name = typeof maybe.name === 'string' ? maybe.name : 'Error';
    const message = typeof maybe.message === 'string' ? maybe.message : String(err);
    const safeName = formatOneLineUtf8(name, MAX_ERROR_NAME_BYTES) || 'Error';
    const safeMessage = formatOneLineUtf8(message, MAX_ERROR_MESSAGE_BYTES) || 'Error';
    return { name: safeName, message: safeMessage };
  }
  const message = formatOneLineUtf8(String(err), MAX_ERROR_MESSAGE_BYTES) || 'Error';
  return { name: 'Error', message };
}

type RequestDeviceProbeResult = {
  ok: boolean;
  device?: Record<string, unknown> | null;
  error?: { name: string; message: string };
  timeoutMs?: number;
};

let requestDeviceProbe: Promise<RequestDeviceProbeResult> | null = null;

function summarizeUsbDevice(
  device: unknown,
): Record<string, unknown> | null {
  if (!device || typeof device !== 'object') return null;

  const out: Record<string, unknown> = {};
  const fieldErrors: Record<string, { name: string; message: string }> = {};

  const read = (field: string) => {
    try {
      out[field] = (device as Record<string, unknown>)[field];
    } catch (err) {
      fieldErrors[field] = serializeError(err);
    }
  };

  // Best-effort metadata; some fields may throw unless the device is open.
  read('vendorId');
  read('productId');
  read('productName');
  read('serialNumber');

  // Extra context (not required for the probe goal, but useful in reports).
  read('manufacturerName');
  read('opened');

  if (Object.keys(fieldErrors).length) out.fieldErrors = fieldErrors;
  return out;
}

async function runRequestDeviceProbe(
  usb: Pick<USB, 'requestDevice'>,
  timeoutMs = 500,
): Promise<RequestDeviceProbeResult> {
  if (requestDeviceProbe) return await requestDeviceProbe;

  requestDeviceProbe = (async () => {
    const settle = Promise.resolve()
      .then(() => usb.requestDevice({ filters: [{ vendorId: 0 }] }))
      .then((device: unknown) => ({ ok: true, device: summarizeUsbDevice(device) } as RequestDeviceProbeResult))
      .catch((err: unknown) => ({ ok: false, error: serializeError(err) } as RequestDeviceProbeResult));

    let timeoutHandle: ReturnType<typeof setTimeout> | null = null;
    const timeout = new Promise<RequestDeviceProbeResult>((resolve) => {
      timeoutHandle = setTimeout(() => {
        resolve({
          ok: false,
          timeoutMs,
          error: {
            name: 'TimeoutError',
            message:
              `navigator.usb.requestDevice() did not settle within ${timeoutMs}ms. ` +
              'If a chooser prompt appeared in the worker context, dismiss it to allow the promise to settle.',
          },
        });
      }, timeoutMs);
      (timeoutHandle as unknown as { unref?: () => void }).unref?.();
    });

    const result = await Promise.race([settle, timeout]);
    if (timeoutHandle !== null) {
      clearTimeout(timeoutHandle);
    }
    // If we timed out, don't cache the result so the user can retry after
    // dismissing any chooser UI that might have appeared.
    if (result.timeoutMs !== undefined) {
      requestDeviceProbe = null;
    }
    return result;
  })();

  return await requestDeviceProbe;
}

async function probe(): Promise<unknown> {
  type UsbMaybe = { getDevices?: unknown; requestDevice?: unknown };
  const usb = (navigator as unknown as { usb?: UsbMaybe }).usb;
  const hasUsb = typeof usb !== 'undefined';

  const report: Record<string, unknown> = {
    isSecureContext: (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true,
    hasUsb,
    hasGetDevices: typeof usb?.getDevices === 'function',
    hasRequestDevice: typeof usb?.requestDevice === 'function',
  };

  if (typeof usb?.getDevices === 'function') {
    try {
      const devices = await (usb.getDevices as () => Promise<unknown>)();
      report.getDevices = {
        ok: true,
        count: Array.isArray(devices) ? devices.length : null,
        devices: Array.isArray(devices) ? devices.map(summarizeUsbDevice) : null,
      };
    } catch (err) {
      report.getDevices = {
        ok: false,
        error: serializeError(err),
      };
    }
  }

  if (typeof usb?.requestDevice === 'function') {
    // In specs/Chromium, this is expected to reject in workers due to missing transient user activation.
    // Use a short timeout so the probe doesn't hang if a chooser prompt appears.
    report.requestDevice = await runRequestDeviceProbe(usb as unknown as Pick<USB, 'requestDevice'>);
  }

  return report;
}

async function matchAndOpen(criteria: {
  vendorId: number;
  productId: number;
  serialNumber?: string;
}): Promise<unknown> {
  type UsbMaybe = { getDevices?: unknown };
  const usb = (navigator as unknown as { usb?: UsbMaybe }).usb;
  const hasUsb = typeof usb !== 'undefined';

  const report: Record<string, unknown> = {
    isSecureContext: (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === true,
    hasUsb,
    criteria,
    hasGetDevices: typeof usb?.getDevices === 'function',
  };

  if (!usb || typeof usb.getDevices !== 'function') {
    report.error = {
      name: 'Error',
      message: 'navigator.usb.getDevices is unavailable in the worker context.',
    };
    return report;
  }

  let devices: unknown[];
  try {
    const res = await (usb.getDevices as () => Promise<unknown>)();
    if (!Array.isArray(res)) {
      report.getDevices = {
        ok: false,
        error: { name: 'TypeError', message: 'navigator.usb.getDevices() did not return an array.' },
      };
      return report;
    }
    devices = res;
  } catch (err) {
    report.getDevices = { ok: false, error: serializeError(err) };
    return report;
  }

  const summaries = devices.map((d) => summarizeUsbDevice(d));
  report.getDevices = { ok: true, count: devices.length, devices: summaries };

  const vendorProductMatches: number[] = [];
  const strictMatches: number[] = [];

  for (let i = 0; i < devices.length; i += 1) {
    const device = devices[i] as { vendorId?: unknown; productId?: unknown; serialNumber?: unknown };
    let vendorIdValue: unknown;
    let productIdValue: unknown;
    try {
      vendorIdValue = device.vendorId;
      productIdValue = device.productId;
    } catch {
      continue;
    }

    if (vendorIdValue !== criteria.vendorId) continue;
    if (productIdValue !== criteria.productId) continue;
    vendorProductMatches.push(i);

    if (criteria.serialNumber === undefined) continue;
    try {
      if (device.serialNumber === criteria.serialNumber) strictMatches.push(i);
    } catch {
      // ignore serial number read errors for strict matching
    }
  }

  let matchIndex: number | null = null;
  let matchKind: 'strict' | 'vendor_product' | 'vendor_product_fallback' | null = null;
  if (criteria.serialNumber !== undefined) {
    if (strictMatches.length) {
      matchIndex = strictMatches[0];
      matchKind = 'strict';
    } else if (vendorProductMatches.length) {
      matchIndex = vendorProductMatches[0];
      matchKind = 'vendor_product_fallback';
    }
  } else if (vendorProductMatches.length) {
    matchIndex = vendorProductMatches[0];
    matchKind = 'vendor_product';
  }

  report.matched = matchIndex !== null;
  report.matchIndex = matchIndex;
  report.matchKind = matchKind;
  report.matchedDevice = matchIndex === null ? null : summaries[matchIndex];

  if (matchIndex === null) return report;

  const device = devices[matchIndex] as { open?: unknown; close?: unknown; opened?: unknown };
  const openedBefore = typeof device.opened === 'boolean' ? (device.opened as boolean) : null;
  let openOk = false;
  let closeOk = false;
  let openError: unknown = null;
  let closeError: unknown = null;

  try {
    if (typeof device.open !== 'function') throw new TypeError('device.open is not a function');
    await (device.open as () => Promise<void>)();
    openOk = true;
  } catch (err) {
    openError = serializeError(err);
  }

  try {
    if (typeof device.close !== 'function') throw new TypeError('device.close is not a function');
    await (device.close as () => Promise<void>)();
    closeOk = true;
  } catch (err) {
    closeError = serializeError(err);
  }

  const openedAfter = typeof device.opened === 'boolean' ? (device.opened as boolean) : null;
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

async function probeDevice(
  device: unknown,
): Promise<unknown> {
  const dev = device as { open?: unknown; close?: unknown; opened?: unknown };
  const report: Record<string, unknown> = {
    received: summarizeUsbDevice(device),
    hasOpen: typeof dev?.open === 'function',
    hasClose: typeof dev?.close === 'function',
  };

  const openedBefore = typeof dev?.opened === 'boolean' ? (dev.opened as boolean) : null;
  let openOk = false;
  let closeOk = false;
  let openError: unknown = null;
  let closeError: unknown = null;

  try {
    if (typeof dev?.open !== 'function') throw new TypeError('device.open is not a function');
    await (dev.open as () => Promise<void>)();
    openOk = true;
  } catch (err) {
    openError = serializeError(err);
  }

  try {
    if (typeof dev?.close !== 'function') throw new TypeError('device.close is not a function');
    await (dev.close as () => Promise<void>)();
    closeOk = true;
  } catch (err) {
    closeError = serializeError(err);
  }

  const openedAfter = typeof dev?.opened === 'boolean' ? (dev.opened as boolean) : null;
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
          const resp: ProbeResponse = { id: msg.id, type: 'probe-result', report };
          ctx.postMessage(resp);
        })
        .catch((err) => {
          const resp: ProbeResponse = { id: msg.id, type: 'error', error: serializeError(err) };
          ctx.postMessage(resp);
        });
      break;
    }
    case 'match_and_open': {
      void matchAndOpen(msg.criteria)
        .then((report) => {
          const resp: ProbeResponse = { id: msg.id, type: 'match_and_open_result', report };
          ctx.postMessage(resp);
        })
        .catch((err) => {
          const resp: ProbeResponse = { id: msg.id, type: 'error', error: serializeError(err) };
          ctx.postMessage(resp);
        });
      break;
    }
    case 'device': {
      void probeDevice(msg.device)
        .then((report) => {
          const resp: ProbeResponse = { id: msg.id, type: 'device-result', report };
          ctx.postMessage(resp);
        })
        .catch((err) => {
          const resp: ProbeResponse = { id: msg.id, type: 'error', error: serializeError(err) };
          ctx.postMessage(resp);
        });
      break;
    }
    default: {
      const _exhaustive: never = msg;
      const resp: ProbeResponse = {
        id: typeof (msg as { id?: unknown }).id === 'number' ? (msg as { id: number }).id : -1,
        type: 'error',
        error: { name: 'Error', message: `Unknown message: ${(msg as { type?: unknown }).type}` },
      };
      ctx.postMessage(resp);
    }
  }
};
