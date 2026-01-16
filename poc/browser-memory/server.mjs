import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;

const PORT = process.env.PORT ? Number(process.env.PORT) : 8080;
const ROOT = __dirname;

const MIME = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".mjs", "text/javascript; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".wasm", "application/wasm"],
  [".svg", "image/svg+xml"],
]);

const UTF8 = Object.freeze({ encoding: "utf-8" });
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder(UTF8.encoding);

function coerceString(input) {
  try {
    return String(input ?? "");
  } catch {
    return "";
  }
}

function formatOneLineUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  if (maxBytes === 0) return "";

  const buf = new Uint8Array(maxBytes);
  let written = 0;
  let pendingSpace = false;
  for (const ch of coerceString(input)) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = written > 0;
      continue;
    }

    if (pendingSpace) {
      const spaceRes = textEncoder.encodeInto(" ", buf.subarray(written));
      if (spaceRes.written === 0) break;
      written += spaceRes.written;
      pendingSpace = false;
      if (written >= maxBytes) break;
    }

    const res = textEncoder.encodeInto(ch, buf.subarray(written));
    if (res.written === 0) break;
    written += res.written;
    if (written >= maxBytes) break;
  }
  return written === 0 ? "" : textDecoder.decode(buf.subarray(0, written));
}

function formatErrorForResponse(err) {
  let message = "";
  if (err && typeof err === "object") {
    try {
      const m = err.message;
      if (typeof m === "string") message = m;
    } catch {
      message = "";
    }
  } else if (typeof err === "string") {
    message = err;
  } else if (
    typeof err === "number" ||
    typeof err === "boolean" ||
    typeof err === "bigint" ||
    typeof err === "symbol" ||
    typeof err === "undefined"
  ) {
    message = String(err);
  } else if (err === null) {
    message = "null";
  }
  return formatOneLineUtf8(message || "Error", 512) || "Error";
}

function send(res, statusCode, body, contentType = "text/plain; charset=utf-8") {
  res.statusCode = statusCode;
  res.setHeader("Content-Type", contentType);
  res.end(body);
}

function safeResolve(rootDir, requestPath) {
  const rootResolved = path.resolve(rootDir);
  const resolved = path.resolve(rootResolved, `.${requestPath}`);
  if (!resolved.startsWith(rootResolved + path.sep) && resolved !== rootResolved) {
    return null;
  }
  return resolved;
}

const server = http.createServer((req, res) => {
  // Required for SharedArrayBuffer/crossOriginIsolated.
  res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
  res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  res.setHeader("Cross-Origin-Resource-Policy", "same-origin");
  res.setHeader("Cache-Control", "no-store");

  const rawUrl = req.url ?? "/";
  if (typeof rawUrl !== "string") return send(res, 400, "Bad Request");
  if (rawUrl.length > MAX_REQUEST_URL_LEN) return send(res, 414, "URI Too Long");

  let url;
  try {
    url = new URL(rawUrl, "http://localhost");
  } catch {
    return send(res, 400, "Bad Request");
  }
  if (url.pathname.length > MAX_PATHNAME_LEN) return send(res, 414, "URI Too Long");

  let pathname;
  try {
    pathname = decodeURIComponent(url.pathname);
  } catch {
    return send(res, 400, "Bad Request");
  }
  if (pathname.length > MAX_PATHNAME_LEN) return send(res, 414, "URI Too Long");
  if (pathname.includes("\0")) return send(res, 400, "Bad Request");
  if (pathname === "/") pathname = "/index.html";

  const filePath = safeResolve(ROOT, pathname);
  if (!filePath) return send(res, 403, "Forbidden");

  let stat;
  try {
    stat = fs.statSync(filePath);
  } catch {
    return send(res, 404, "Not Found");
  }

  if (stat.isDirectory()) {
    return send(res, 404, "Not Found");
  }

  const ext = path.extname(filePath);
  res.setHeader("Content-Type", MIME.get(ext) ?? "application/octet-stream");

  const stream = fs.createReadStream(filePath);
  stream.on("error", (err) => send(res, 500, `Server error: ${formatErrorForResponse(err)}`));
  stream.pipe(res);
});

server.listen(PORT, () => {
  // eslint-disable-next-line no-console
  console.log(`Aero browser-memory PoC server running at http://localhost:${PORT}/`);
  // eslint-disable-next-line no-console
  console.log("This server sets COOP/COEP headers so SharedArrayBuffer is available.");
});
