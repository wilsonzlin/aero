import http from "node:http";
import fs from "node:fs";
import * as fsp from "node:fs/promises";
import path from "node:path";
import { pipeline } from "node:stream/promises";
import { formatOneLineError, formatOneLineUtf8 } from "../src/text.js";
import { isExpectedStreamAbort } from "../src/stream_abort.js";
import { destroyBestEffort } from "../src/socket_safe.js";
import { tryWriteResponse } from "../src/http_response_safe.js";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_ERROR_BODY_BYTES = 512;

const CONTENT_TYPES = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".wasm", "application/wasm"],
  [".png", "image/png"],
  [".jpg", "image/jpeg"],
  [".jpeg", "image/jpeg"],
  [".svg", "image/svg+xml; charset=utf-8"],
]);

function contentType(filePath) {
  return CONTENT_TYPES.get(path.extname(filePath).toLowerCase()) ?? "application/octet-stream";
}

function safeResolve(rootDir, requestPath) {
  const resolved = path.resolve(rootDir, "." + requestPath);
  const rootResolved = path.resolve(rootDir);
  if (!resolved.startsWith(rootResolved + path.sep) && resolved !== rootResolved) {
    return null;
  }
  return resolved;
}

function sendText(res, statusCode, text, extraHeaders) {
  const safeText = formatOneLineUtf8(text, MAX_ERROR_BODY_BYTES) || "Error";
  const body = Buffer.from(safeText, "utf8");
  const headers = {
    "Content-Type": "text/plain; charset=utf-8",
    "Content-Length": body.length,
    "Cache-Control": "no-store",
    ...extraHeaders,
  };
  tryWriteResponse(
    res,
    statusCode,
    headers,
    body,
  );
}

function pipeFile(res, filePath) {
  const stream = fs.createReadStream(filePath);
  void pipeline(stream, res).catch((err) => {
    if (isExpectedStreamAbort(err)) return;
    // Avoid echoing internal errors back to clients; logs are sufficient for debugging.
    // eslint-disable-next-line no-console
    console.error(`bench static server: stream error: ${formatOneLineError(err, 512, "Error")}`);
    // Defensive: avoid relying on response state getters (they can throw on hostile/monkeypatched
    // objects). Best-effort emit a 500; `sendText` will destroy on write failure.
    sendText(res, 500, "Internal server error");
  });
}

/**
 * @param {{ rootDir: string, host?: string, port?: number }} opts
 * @returns {Promise<{ baseUrl: string, close: () => Promise<void> }>}
 */
export async function startStaticServer(opts) {
  const host = opts.host ?? "127.0.0.1";
  const port = opts.port ?? 0;
  const rootDir = opts.rootDir;

  const server = http.createServer(async (req, res) => {
    try {
      const method = req.method ?? "GET";
      if (method === "OPTIONS") {
        tryWriteResponse(res, 204, {
          Allow: "GET, HEAD, OPTIONS",
          "Cache-Control": "no-store",
          "Content-Length": "0",
        });
        return;
      }
      if (method !== "GET" && method !== "HEAD") {
        sendText(res, 405, "Method Not Allowed", { Allow: "GET, HEAD, OPTIONS" });
        return;
      }

      const rawUrl = req.url ?? "/";
      if (typeof rawUrl !== "string") {
        sendText(res, 400, "Bad Request");
        return;
      }
      if (rawUrl.length > MAX_REQUEST_URL_LEN) {
        sendText(res, 414, "URI Too Long");
        return;
      }

      let requestUrl;
      try {
        // Base URL is only used to parse the request target; avoid tying it to the listen host.
        requestUrl = new URL(rawUrl, "http://localhost");
      } catch {
        sendText(res, 400, "Bad Request");
        return;
      }
      if (requestUrl.pathname.length > MAX_PATHNAME_LEN) {
        sendText(res, 414, "URI Too Long");
        return;
      }

      let pathname;
      try {
        pathname = decodeURIComponent(requestUrl.pathname);
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
      if (pathname === "/") pathname = "/index.html";

      const filePath = safeResolve(rootDir, pathname);
      if (!filePath) {
        sendText(res, 403, "Forbidden");
        return;
      }
      const st = await fsp.stat(filePath).catch(() => null);
      if (!st || !st.isFile()) {
        sendText(res, 404, "Not found");
        return;
      }

      const headers = {
        "Content-Type": contentType(filePath),
        "Content-Length": st.size,
      };
      if (method === "HEAD") {
        tryWriteResponse(res, 200, headers);
        return;
      }
      try {
        res.writeHead(200, headers);
      } catch {
        destroyBestEffort(res);
        return;
      }
      pipeFile(res, filePath);
    } catch (err) {
      // Avoid echoing internal errors (and any attacker-controlled strings) back to clients.
      // This server is dev-only; logs are sufficient for debugging.
      // eslint-disable-next-line no-console
      console.error(`bench static server: handler error: ${formatOneLineError(err, 512, "Error")}`);
      // Defensive: avoid relying on response state getters. Best-effort emit a 500; `sendText`
      // will destroy the response on write failure.
      sendText(res, 500, "Internal server error");
    }
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => resolve());
  });

  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Unexpected server address");

  return {
    baseUrl: `http://${host}:${address.port}/`,
    close: () =>
      new Promise((resolve, reject) => {
        server.close((err) => (err ? reject(err) : resolve()));
      })
  };
}
