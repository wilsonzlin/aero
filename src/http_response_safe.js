import { destroyBestEffort } from "./socket_safe.js";

function readContentTypeBestEffort(opts, fallback) {
  let contentType;
  try {
    contentType = opts?.contentType;
  } catch {
    // ignore
  }
  if (typeof contentType !== "string" || contentType.length === 0) return fallback;
  return contentType;
}

function isValidRawHeaders(headers) {
  if (!Array.isArray(headers)) return false;
  if (headers.length === 0) return true;
  if (headers.length % 2 !== 0) return false;
  for (let i = 0; i < headers.length; i += 2) {
    if (typeof headers[i] !== "string") return false;
    const value = headers[i + 1];
    if (typeof value === "string" || typeof value === "number") continue;
    if (!Array.isArray(value)) return false;
    for (const item of value) {
      if (typeof item !== "string") return false;
    }
  }
  return true;
}

export function tryWriteResponse(res, statusCode, headers, body) {
  try {
    if (headers && typeof headers === "object" && !Array.isArray(headers)) {
      res.writeHead(statusCode, headers);
    } else if (Array.isArray(headers) && headers.length > 0 && isValidRawHeaders(headers)) {
      res.writeHead(statusCode, headers);
    } else {
      res.writeHead(statusCode);
    }

    if (body === undefined) {
      res.end();
    } else {
      res.end(body);
    }
  } catch {
    destroyBestEffort(res);
  }
}

export function sendJsonNoStore(res, statusCode, value, opts) {
  let code = statusCode;
  let contentType = readContentTypeBestEffort(opts, "application/json; charset=utf-8");
  let body = "";
  try {
    body = JSON.stringify(value);
  } catch {
    code = 500;
    contentType = "application/json; charset=utf-8";
    // Do not call JSON.stringify again; treat it as hostile/unavailable.
    body = `{"error":"internal server error"}`;
  }

  tryWriteResponse(
    res,
    code,
    {
      "content-type": contentType,
      "content-length": Buffer.byteLength(body),
      "cache-control": "no-store",
    },
    body,
  );
}

export function sendTextNoStore(res, statusCode, body, opts) {
  const contentType = readContentTypeBestEffort(opts, "text/plain; charset=utf-8");
  tryWriteResponse(
    res,
    statusCode,
    {
      "content-type": contentType,
      "content-length": Buffer.byteLength(body),
      "cache-control": "no-store",
    },
    body,
  );
}

