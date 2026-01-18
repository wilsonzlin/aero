import { tryGetProp } from "./safe_props.js";

function tryGetMethod(obj, key) {
  const value = tryGetProp(obj, key);
  return typeof value === "function" ? value : null;
}

export function tryGetMethodBestEffort(obj, key) {
  return tryGetMethod(obj, key);
}

function missingMethodError(key) {
  return new Error(`Missing required method: ${key}`);
}

function callMethodRequired(obj, key, ...args) {
  const fn = tryGetMethod(obj, key);
  if (!fn) return false;
  try {
    fn.apply(obj, args);
    return true;
  } catch {
    return false;
  }
}

function callMethodOptional(obj, key, ...args) {
  const fn = tryGetMethod(obj, key);
  if (!fn) return true;
  try {
    fn.apply(obj, args);
    return true;
  } catch {
    return false;
  }
}

export function callMethodBestEffort(obj, key, ...args) {
  return callMethodOptional(obj, key, ...args);
}

function callMethodCaptureError(obj, key, ...args) {
  const fn = tryGetMethod(obj, key);
  if (!fn) return missingMethodError(key);
  try {
    fn.apply(obj, args);
    return null;
  } catch (err) {
    return err;
  }
}

export function callMethodCaptureErrorBestEffort(obj, key, ...args) {
  return callMethodCaptureError(obj, key, ...args);
}

export function writeCaptureErrorBestEffort(stream, ...args) {
  const fn = tryGetMethod(stream, "write");
  if (!fn) return { ok: false, err: missingMethodError("write") };
  try {
    const result = fn.apply(stream, args);
    return { ok: !!result, err: null };
  } catch (err) {
    return { ok: false, err };
  }
}

export function destroyBestEffort(obj) {
  callMethodOptional(obj, "destroy");
}

export function destroyWithErrorBestEffort(obj, err) {
  callMethodOptional(obj, "destroy", err);
}

export function closeBestEffort(obj, ...args) {
  callMethodOptional(obj, "close", ...args);
}

export function pauseBestEffort(stream) {
  callMethodOptional(stream, "pause");
}

export function resumeBestEffort(stream) {
  callMethodOptional(stream, "resume");
}

export function pauseRequired(stream) {
  return callMethodRequired(stream, "pause");
}

export function resumeRequired(stream) {
  return callMethodRequired(stream, "resume");
}

export function endBestEffort(stream, ...args) {
  callMethodOptional(stream, "end", ...args);
}

export function endRequired(stream, ...args) {
  return callMethodRequired(stream, "end", ...args);
}

export function endCaptureErrorBestEffort(stream, ...args) {
  return callMethodCaptureError(stream, "end", ...args);
}

export function removeAllListenersBestEffort(emitter) {
  callMethodOptional(emitter, "removeAllListeners");
}

export function setNoDelayBestEffort(socket, noDelay) {
  callMethodOptional(socket, "setNoDelay", noDelay);
}

export function setNoDelayRequired(socket, noDelay) {
  return callMethodRequired(socket, "setNoDelay", noDelay);
}

export function setTimeoutRequired(socket, timeoutMs) {
  return callMethodRequired(socket, "setTimeout", timeoutMs);
}

