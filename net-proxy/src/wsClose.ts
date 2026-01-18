import type { WebSocket } from "ws";

import { formatOneLineUtf8 } from "./text";

export type WsSendData = Buffer | ArrayBuffer | ArrayBufferView | string;

export function wsIsOpenSafe(ws: WebSocket | null | undefined): boolean {
  if (!ws) return false;
  let OPEN = 1;
  try {
    const maybeOpen = (ws as unknown as { OPEN?: unknown }).OPEN;
    if (typeof maybeOpen === "number") OPEN = maybeOpen;
  } catch {
    // ignore (use default OPEN=1)
  }
  try {
    const maybeReadyState = (ws as unknown as { readyState?: unknown }).readyState;
    return typeof maybeReadyState === "number" ? maybeReadyState === OPEN : false;
  } catch {
    return false;
  }
}

export function wsSendSafe(ws: WebSocket, data: WsSendData, cb?: (err?: Error) => void): boolean {
  let sendFn: ((...args: unknown[]) => unknown) | undefined;
  try {
    sendFn = (ws as unknown as { send?: unknown } | null | undefined)?.send as ((...args: unknown[]) => unknown) | undefined;
  } catch {
    sendFn = undefined;
  }

  if (!ws || typeof sendFn !== "function") {
    if (typeof cb === "function") queueMicrotask(() => cb(new Error("Invalid WebSocket")));
    return false;
  }

  let OPEN = 1;
  try {
    const maybeOpen = (ws as unknown as { OPEN?: unknown }).OPEN;
    if (typeof maybeOpen === "number") OPEN = maybeOpen;
  } catch {
    // ignore (use default OPEN=1)
  }
  try {
    const maybeReadyState = (ws as unknown as { readyState?: unknown }).readyState;
    if (typeof maybeReadyState === "number" && maybeReadyState !== OPEN) {
      if (typeof cb === "function") queueMicrotask(() => cb(new Error("WebSocket not open")));
      return false;
    }
  } catch {
    if (typeof cb === "function") queueMicrotask(() => cb(new Error("WebSocket not open")));
    return false;
  }

  const cbSafe =
    typeof cb === "function"
      ? (err?: unknown) => {
          // Some callback-style send implementations follow Node convention: cb(null) on success.
          cb(err == null ? undefined : err instanceof Error ? err : new Error("WebSocket error"));
        }
      : undefined;

  try {
    if (cbSafe) {
      // ws-style APIs often support callbacks, but some implementations use a rest-arg signature
      // (arity 1) while still accepting a callback. Heuristic: treat `.terminate()` as a ws-style
      // indicator and allow passing the callback.
      let hasTerminate = false;
      try {
        hasTerminate = typeof (ws as unknown as { terminate?: unknown }).terminate === "function";
      } catch {
        hasTerminate = false;
      }
      if (sendFn.length >= 2 || hasTerminate) {
        sendFn.call(ws, data, cbSafe);
      } else {
        sendFn.call(ws, data);
        queueMicrotask(() => cbSafe());
      }
    } else {
      sendFn.call(ws, data);
    }
    return true;
  } catch (err) {
    if (cbSafe) queueMicrotask(() => cbSafe(err));
    try {
      (ws as unknown as { terminate?: () => void }).terminate?.();
    } catch {
      // ignore
    }
    return false;
  }
}

export function wsCloseSafe(ws: WebSocket, code?: number, reason?: unknown): void {
  let closeFn: ((...args: unknown[]) => unknown) | undefined;
  try {
    closeFn = (ws as unknown as { close?: unknown } | null | undefined)?.close as ((...args: unknown[]) => unknown) | undefined;
  } catch {
    closeFn = undefined;
  }
  if (!ws || typeof closeFn !== "function") return;
  try {
    if (typeof code !== "number") {
      closeFn.call(ws);
      return;
    }
    if (reason === undefined) {
      closeFn.call(ws, code);
      return;
    }
    const safeReason = formatOneLineUtf8(reason, 123);
    if (!safeReason) {
      closeFn.call(ws, code);
      return;
    }
    closeFn.call(ws, code, safeReason);
  } catch {
    try {
      (ws as unknown as { terminate?: () => void }).terminate?.();
    } catch {
      // ignore
    }
  }
}

