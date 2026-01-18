const { tryGetProp } = require("./safe_props.cjs");

function tryGetMethod(obj, key) {
  const value = tryGetProp(obj, key);
  return typeof value === "function" ? value : null;
}

function tryGetMethodBestEffort(obj, key) {
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

function callMethodBestEffort(obj, key, ...args) {
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

function callMethodCaptureErrorBestEffort(obj, key, ...args) {
  return callMethodCaptureError(obj, key, ...args);
}

function writeCaptureErrorBestEffort(stream, ...args) {
  const fn = tryGetMethod(stream, "write");
  if (!fn) return { ok: false, err: missingMethodError("write") };
  try {
    const result = fn.apply(stream, args);
    return { ok: !!result, err: null };
  } catch (err) {
    return { ok: false, err };
  }
}

function destroyBestEffort(obj) {
  callMethodOptional(obj, "destroy");
}

function destroyWithErrorBestEffort(obj, err) {
  callMethodOptional(obj, "destroy", err);
}

function closeBestEffort(obj, ...args) {
  callMethodOptional(obj, "close", ...args);
}

function pauseBestEffort(stream) {
  callMethodOptional(stream, "pause");
}

function resumeBestEffort(stream) {
  callMethodOptional(stream, "resume");
}

function pauseRequired(stream) {
  return callMethodRequired(stream, "pause");
}

function resumeRequired(stream) {
  return callMethodRequired(stream, "resume");
}

function endBestEffort(stream, ...args) {
  callMethodOptional(stream, "end", ...args);
}

function endRequired(stream, ...args) {
  return callMethodRequired(stream, "end", ...args);
}

function endCaptureErrorBestEffort(stream, ...args) {
  return callMethodCaptureError(stream, "end", ...args);
}

function removeAllListenersBestEffort(emitter) {
  callMethodOptional(emitter, "removeAllListeners");
}

function setNoDelayBestEffort(socket, noDelay) {
  callMethodOptional(socket, "setNoDelay", noDelay);
}

function setNoDelayRequired(socket, noDelay) {
  return callMethodRequired(socket, "setNoDelay", noDelay);
}

function setTimeoutRequired(socket, timeoutMs) {
  return callMethodRequired(socket, "setTimeout", timeoutMs);
}

module.exports = {
  tryGetMethodBestEffort,
  callMethodBestEffort,
  callMethodCaptureErrorBestEffort,
  writeCaptureErrorBestEffort,
  destroyBestEffort,
  destroyWithErrorBestEffort,
  closeBestEffort,
  pauseBestEffort,
  resumeBestEffort,
  pauseRequired,
  resumeRequired,
  endBestEffort,
  endRequired,
  endCaptureErrorBestEffort,
  removeAllListenersBestEffort,
  setNoDelayBestEffort,
  setNoDelayRequired,
  setTimeoutRequired,
};

