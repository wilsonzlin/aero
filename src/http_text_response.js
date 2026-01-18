import { Buffer } from "node:buffer";

export function encodeHttpTextResponse(opts) {
  const statusCode = opts?.statusCode;
  const statusText = opts?.statusText;
  const bodyText = opts?.bodyText ?? "";

  if (!Number.isInteger(statusCode) || statusCode < 100 || statusCode > 999) {
    throw new RangeError("encodeHttpTextResponse: statusCode must be a 3-digit integer");
  }
  if (typeof statusText !== "string" || statusText.trim() === "") {
    throw new TypeError("encodeHttpTextResponse: statusText must be a non-empty string");
  }
  if (statusText.includes("\r") || statusText.includes("\n")) {
    throw new TypeError("encodeHttpTextResponse: statusText must not contain CR/LF");
  }
  if (typeof bodyText !== "string") {
    throw new TypeError("encodeHttpTextResponse: bodyText must be a string");
  }

  const contentType = opts?.contentType ?? "text/plain; charset=utf-8";
  if (typeof contentType !== "string" || contentType.trim() === "") {
    throw new TypeError("encodeHttpTextResponse: contentType must be a non-empty string");
  }
  if (contentType.includes("\r") || contentType.includes("\n")) {
    throw new TypeError("encodeHttpTextResponse: contentType must not contain CR/LF");
  }

  let cacheControl = "no-store";
  if (opts && Object.prototype.hasOwnProperty.call(opts, "cacheControl")) {
    cacheControl = opts.cacheControl;
  }
  if (cacheControl !== null) {
    if (typeof cacheControl !== "string") {
      throw new TypeError("encodeHttpTextResponse: cacheControl must be a string or null");
    }
    if (cacheControl.trim() === "") {
      throw new TypeError("encodeHttpTextResponse: cacheControl must be a non-empty string or null");
    }
    if (cacheControl.includes("\r") || cacheControl.includes("\n")) {
      throw new TypeError("encodeHttpTextResponse: cacheControl must not contain CR/LF");
    }
  }

  const closeConnection = opts?.closeConnection ?? true;
  if (typeof closeConnection !== "boolean") {
    throw new TypeError("encodeHttpTextResponse: closeConnection must be a boolean");
  }

  const body = Buffer.from(bodyText, "utf8");
  const headers = [
    `HTTP/1.1 ${statusCode} ${statusText}`,
    `Content-Type: ${contentType}`,
    `Content-Length: ${body.length}`,
    ...(cacheControl ? [`Cache-Control: ${cacheControl}`] : []),
    ...(closeConnection ? ["Connection: close"] : []),
    "",
    "",
  ].join("\r\n");

  return Buffer.concat([Buffer.from(headers, "utf8"), body]);
}
