import { Buffer } from "node:buffer";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { formatOneLineUtf8 } from "../src/text.js";
import { tryWriteResponse } from "../src/http_response_safe.js";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_ERROR_BODY_BYTES = 512;

function safeTextBody(message) {
  return formatOneLineUtf8(message, MAX_ERROR_BODY_BYTES) || "Error";
}

const webRoot = resolve(fileURLToPath(new URL(".", import.meta.url)));

const mimeTypes = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
};

const SAB_HEADERS = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
};

function safeResolve(rootDir, requestPath) {
  const rootResolved = resolve(rootDir);
  const resolved = resolve(rootResolved, `.${requestPath}`);
  if (!resolved.startsWith(rootResolved + sep) && resolved !== rootResolved) return null;
  return resolved;
}

function sendText(res, statusCode, message) {
  const body = Buffer.from(safeTextBody(message), "utf8");
  tryWriteResponse(
    res,
    statusCode,
    {
      ...SAB_HEADERS,
      "Content-Type": "text/plain; charset=utf-8",
      "Content-Length": String(body.byteLength),
    },
    body,
  );
}

function sendBytes(res, statusCode, body, contentType) {
  tryWriteResponse(
    res,
    statusCode,
    {
      ...SAB_HEADERS,
      "Content-Type": contentType,
      "Content-Length": String(body.byteLength),
    },
    body,
  );
}

function isNodeErrorWithCode(err) {
  if (!err || typeof err !== "object") return false;
  try {
    return typeof err.code === "string";
  } catch {
    return false;
  }
}

function isMissingFileError(err) {
  if (!isNodeErrorWithCode(err)) return false;
  return err.code === "ENOENT" || err.code === "ENOTDIR";
}

const server = createServer(async (req, res) => {
  const rawUrl = req.url ?? "/";
  if (typeof rawUrl !== "string") {
    sendText(res, 400, "Bad Request");
    return;
  }
  if (rawUrl.length > MAX_REQUEST_URL_LEN) {
    sendText(res, 414, "URI Too Long");
    return;
  }

  let url;
  try {
    url = new URL(rawUrl, "http://localhost");
  } catch {
    sendText(res, 400, "Bad Request");
    return;
  }
  if (url.pathname.length > MAX_PATHNAME_LEN) {
    sendText(res, 414, "URI Too Long");
    return;
  }

  let pathname;
  try {
    pathname = decodeURIComponent(url.pathname);
  } catch {
    sendText(res, 400, "Bad Request");
    return;
  }
  if (pathname.length > MAX_PATHNAME_LEN) {
    sendText(res, 414, "URI Too Long");
    return;
  }
  if (pathname.includes("\0")) {
    sendText(res, 400, "Bad Request");
    return;
  }

  if (pathname === "/") pathname = "/virtio-snd-smoke-test.html";
  const abs = safeResolve(webRoot, pathname);
  if (!abs) {
    sendText(res, 404, "Not found");
    return;
  }

  let data;
  try {
    data = await readFile(abs);
  } catch (err) {
    if (isMissingFileError(err)) sendText(res, 404, "Not found");
    else sendText(res, 500, "Internal server error");
    return;
  }

  sendBytes(res, 200, data, mimeTypes[extname(abs)] ?? "application/octet-stream");
});

const port = Number(process.env.PORT ?? 8000);
server.listen(port, () => {
  // eslint-disable-next-line no-console
  console.log(`Serving ${webRoot} on http://localhost:${port}/`);
});
