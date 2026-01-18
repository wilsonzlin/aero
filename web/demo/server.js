import http from "node:http";
import { Buffer } from "node:buffer";
import { promises as fs } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatOneLineUtf8 } from "../../src/text.js";
import { tryWriteResponse } from "../../src/http_response_safe.js";
import { tryGetProp } from "../../src/safe_props.js";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_ERROR_BODY_BYTES = 512;

function safeTextBody(message) {
  return formatOneLineUtf8(message, MAX_ERROR_BODY_BYTES) || "Error";
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../..");

const port = Number(process.env.PORT ?? 8000);

const SAB_HEADERS = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
  // Convenient for local files fetched by the demo.
  "Cross-Origin-Resource-Policy": "cross-origin",
};

function contentTypeFor(filePath) {
  if (filePath.endsWith(".html")) return "text/html; charset=utf-8";
  if (filePath.endsWith(".js")) return "text/javascript; charset=utf-8";
  if (filePath.endsWith(".css")) return "text/css; charset=utf-8";
  if (filePath.endsWith(".json")) return "application/json; charset=utf-8";
  return "application/octet-stream";
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
      "Cache-Control": "no-store",
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
      "Cache-Control": "no-store",
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

function safeResolve(rootDir, requestPath) {
  const rootResolved = path.resolve(rootDir);
  const resolved = path.resolve(rootResolved, `.${requestPath}`);
  if (!resolved.startsWith(rootResolved + path.sep) && resolved !== rootResolved) {
    return null;
  }
  return resolved;
}

const server = http.createServer(async (req, res) => {
  const rawUrl = tryGetProp(req, "url");
  if (typeof rawUrl !== "string" || rawUrl === "") {
    sendText(res, 400, "Bad Request");
    return;
  }
  if (rawUrl.length > MAX_REQUEST_URL_LEN) {
    sendText(res, 414, "URI Too Long");
    return;
  }
  if (rawUrl.trim() !== rawUrl) {
    sendText(res, 400, "Bad Request");
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

  let decodedPath;
  try {
    decodedPath = decodeURIComponent(url.pathname);
  } catch {
    sendText(res, 400, "Bad Request");
    return;
  }
  if (decodedPath.length > MAX_PATHNAME_LEN) {
    sendText(res, 414, "URI Too Long");
    return;
  }
  if (decodedPath.includes("\0")) {
    sendText(res, 400, "Bad Request");
    return;
  }

  // Serve from repo root, but prevent directory traversal.
  let filePath = safeResolve(repoRoot, decodedPath);
  if (!filePath) {
    sendText(res, 403, "Forbidden");
    return;
  }

  let stat;
  try {
    stat = await fs.stat(filePath);
  } catch (err) {
    if (isMissingFileError(err)) sendText(res, 404, "Not Found");
    else sendText(res, 500, "Internal server error");
    return;
  }
  if (stat.isDirectory()) {
    filePath = path.join(filePath, "index.html");
    try {
      stat = await fs.stat(filePath);
    } catch (err) {
      if (isMissingFileError(err)) sendText(res, 404, "Not Found");
      else sendText(res, 500, "Internal server error");
      return;
    }
  }
  if (!stat.isFile()) {
    sendText(res, 404, "Not Found");
    return;
  }

  let data;
  try {
    data = await fs.readFile(filePath);
  } catch (err) {
    if (isMissingFileError(err)) sendText(res, 404, "Not Found");
    else sendText(res, 500, "Internal server error");
    return;
  }

  sendBytes(res, 200, data, contentTypeFor(filePath));
});

server.listen(port, () => {
  // eslint-disable-next-line no-console
  console.log(`Aero perf demo server running at http://localhost:${port}/web/demo/`);
});

