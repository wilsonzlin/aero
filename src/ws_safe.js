import { formatOneLineUtf8 } from "./text.js";

// RFC 6455 close reason is limited to 123 bytes (125 total payload bytes incl. 2-byte code).
const MAX_WS_CLOSE_REASON_BYTES = 123;

export function wsIsOpenSafe(ws) {
  if (ws == null || (typeof ws !== "object" && typeof ws !== "function")) return false;
  let openState = 1;
  try {
    openState = typeof ws.OPEN === "number" ? ws.OPEN : 1;
  } catch {
    // ignore (treat as default)
  }
  try {
    const readyState = ws.readyState;
    if (typeof readyState !== "number") return true;
    return readyState === openState;
  } catch {
    return false;
  }
}

export function wsSendSafe(ws, data, cb) {
  let sendFn;
  try {
    sendFn = ws?.send;
  } catch {
    sendFn = undefined;
  }
  if (!ws || typeof sendFn !== "function") {
    if (typeof cb === "function") queueMicrotask(() => cb(new Error("Invalid WebSocket")));
    return false;
  }
  let openState = 1;
  try {
    openState = typeof ws.OPEN === "number" ? ws.OPEN : 1;
  } catch {
    // ignore (treat as default)
  }
  // If we can observe `readyState`, avoid calling `send()` when not open.
  try {
    if (typeof ws.readyState === "number" && ws.readyState !== openState) {
      if (typeof cb === "function") queueMicrotask(() => cb(new Error("WebSocket not open")));
      return false;
    }
  } catch {
    if (typeof cb === "function") queueMicrotask(() => cb(new Error("WebSocket not open")));
    return false;
  }
  try {
    if (typeof cb === "function") {
      const cbSafe = (err) => {
        // Some callback-style send implementations follow Node convention: cb(null) on success.
        cb(err == null ? undefined : err);
      };
      let hasTerminate = false;
      try {
        hasTerminate = typeof ws.terminate === "function";
      } catch {
        hasTerminate = false;
      }
      // ws-style APIs often support callbacks, but some implementations use a rest-arg signature
      // (arity 1) while still accepting a callback. Heuristic: treat `.terminate()` as a ws-style
      // indicator and allow passing the callback.
      if (sendFn.length >= 2 || hasTerminate) {
        sendFn.call(ws, data, cbSafe);
      } else {
        sendFn.call(ws, data);
        queueMicrotask(() => cb());
      }
    } else {
      sendFn.call(ws, data);
    }
    return true;
  } catch (err) {
    if (typeof cb === "function") queueMicrotask(() => cb(err));
    try {
      ws?.terminate?.();
    } catch {
      // ignore
    }
    return false;
  }
}

export function wsCloseSafe(ws, code, reason) {
  let closeFn;
  try {
    closeFn = ws?.close;
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
    const safeReason = formatOneLineUtf8(reason, MAX_WS_CLOSE_REASON_BYTES);
    if (!safeReason) {
      closeFn.call(ws, code);
      return;
    }
    closeFn.call(ws, code, safeReason);
  } catch {
    try {
      ws?.terminate?.();
    } catch {
      // ignore
    }
  }
}
