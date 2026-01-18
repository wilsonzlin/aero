export type RtcDataChannelSendData = string | ArrayBuffer | ArrayBufferView | Blob;

export function dcIsOpenSafe(dc: RTCDataChannel | null | undefined): boolean {
  if (!dc) return false;
  try {
    return (dc as unknown as { readyState?: unknown }).readyState === "open";
  } catch {
    return false;
  }
}

export function dcIsClosedSafe(dc: RTCDataChannel | null | undefined): boolean {
  if (!dc) return false;
  try {
    return (dc as unknown as { readyState?: unknown }).readyState === "closed";
  } catch {
    return false;
  }
}

export function dcBufferedAmountSafe(dc: RTCDataChannel | null | undefined): number {
  if (!dc) return 0;
  try {
    const value = (dc as unknown as { bufferedAmount?: unknown }).bufferedAmount;
    return Number.isFinite(value) ? (value as number) : 0;
  } catch {
    return 0;
  }
}

export function dcSendSafe(dc: RTCDataChannel | null | undefined, data: RtcDataChannelSendData): boolean {
  if (!dc) return false;
  let sendFn: ((data: unknown) => void) | undefined;
  try {
    sendFn = (dc as unknown as { send?: unknown }).send as ((data: unknown) => void) | undefined;
  } catch {
    sendFn = undefined;
  }
  if (typeof sendFn !== "function") return false;

  if (!dcIsOpenSafe(dc)) return false;
  try {
    // Some lib.dom versions are overly strict about accepted send() types. Runtime accepts
    // the same shapes we already use (ArrayBuffer, ArrayBufferView, string, Blob).
    sendFn.call(dc, data);
    return true;
  } catch {
    return false;
  }
}

export function dcCloseSafe(dc: RTCDataChannel | null | undefined): void {
  if (!dc) return;
  let closeFn: (() => void) | undefined;
  try {
    closeFn = (dc as unknown as { close?: unknown }).close as (() => void) | undefined;
  } catch {
    closeFn = undefined;
  }
  if (typeof closeFn !== "function") return;
  try {
    closeFn.call(dc);
  } catch {
    // ignore
  }
}

export function pcCloseSafe(pc: RTCPeerConnection | null | undefined): void {
  if (!pc) return;
  let closeFn: (() => void) | undefined;
  try {
    closeFn = (pc as unknown as { close?: unknown }).close as (() => void) | undefined;
  } catch {
    closeFn = undefined;
  }
  if (typeof closeFn !== "function") return;
  try {
    closeFn.call(pc);
  } catch {
    // ignore
  }
}
