#!/usr/bin/env node
/**
 * Minimal static server for chunked disk images (manifest.json + chunks/*.bin).
 *
 * Intended for development/testing of Aero's chunked disk streaming backend and the
 * `tools/disk-streaming-conformance` checker in `--mode chunked`.
 *
 * Example:
 *   node server/chunk_server.js --dir ./chunked-image --port 8080 --coop-coep
 */

import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { pipeline } from "node:stream/promises";
import { formatOneLineError, formatOneLineUtf8 } from "./src/text.js";
import { isExpectedStreamAbort } from "../src/stream_abort.js";
import { tryWriteResponse } from "../src/http_response_safe.js";
import { tryGetProp, tryGetStringProp } from "../src/safe_props.js";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_AUTH_HEADER_LEN = 4 * 1024;
const MAX_IF_NONE_MATCH_LEN = 16 * 1024;
const MAX_IF_MODIFIED_SINCE_LEN = 128;
const MAX_ERROR_BODY_BYTES = 512;

function logServerError(prefix, err) {
  // eslint-disable-next-line no-console
  console.error(`${prefix}: ${formatOneLineError(err, 512, "Error")}`);
}

function clearHeaders(res) {
  try {
    for (const name of res.getHeaderNames()) res.removeHeader(name);
  } catch {
    // ignore
  }
}

function trySetHeader(res, name, value) {
  try {
    res.setHeader(name, value);
    return true;
  } catch {
    return false;
  }
}

function safeRequestMethod(req) {
  return tryGetStringProp(req, "method") ?? "GET";
}

function safeHeader(req, name) {
  return tryGetProp(tryGetProp(req, "headers"), name);
}

function finishResponse(res, statusCode, body) {
  // Preserve existing semantics: callers typically pre-populate headers with `setHeader`.
  // `tryWriteResponse(..., headers=null)` flushes those headers via `writeHead(statusCode)` and ends.
  tryWriteResponse(res, statusCode, null, body);
}

function setStatusCodeSafe(res, statusCode) {
  try {
    res.statusCode = statusCode;
    return true;
  } catch {
    return false;
  }
}

function parseArgs(argv) {
  const args = { dir: process.cwd(), port: 8080, coopCoep: false, authToken: null };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--dir") args.dir = argv[++i];
    else if (a === "--port") args.port = Number(argv[++i]);
    else if (a === "--coop-coep") args.coopCoep = true;
    else if (a === "--auth-token") args.authToken = argv[++i];
    else if (a === "--help") args.help = true;
  }
  return args;
}

const args = parseArgs(process.argv);
if (args.help) {
  console.log(
    "Usage: node server/chunk_server.js --dir <root> --port <port> [--coop-coep] [--auth-token <Authorization-value>]",
  );
  process.exit(0);
}

const root = path.resolve(args.dir);

function safeJoin(rootDir, requestPath) {
  let decoded;
  try {
    decoded = decodeURIComponent(requestPath);
  } catch {
    return null;
  }
  // `requestPath` is already capped, but percent-decoding can expand it.
  if (decoded.length > MAX_PATHNAME_LEN) return null;
  if (decoded.includes("\0")) return null;
  const full = path.resolve(path.join(rootDir, "." + decoded));
  if (!full.startsWith(rootDir + path.sep) && full !== rootDir) return null;
  return full;
}

function computeEtag(stat) {
  return `"${stat.size.toString(16)}-${Math.floor(stat.mtimeMs).toString(16)}"`;
}

function stripWeakEtagPrefix(etag) {
  return etag.trim().replace(/^w\//i, "");
}

function ifNoneMatchMatches(ifNoneMatch, currentEtag) {
  if (typeof ifNoneMatch !== "string") return false;
  const raw = ifNoneMatch.trim();
  if (!raw) return false;
  if (raw === "*") return true;

  const current = stripWeakEtagPrefix(currentEtag);
  let start = 0;
  let inQuotes = false;
  let escaped = false;
  for (let i = 0; i < raw.length; i++) {
    const ch = raw[i];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (inQuotes && ch === "\\") {
      escaped = true;
      continue;
    }
    if (ch === '"') {
      inQuotes = !inQuotes;
      continue;
    }
    if (ch === "," && !inQuotes) {
      const tag = raw.slice(start, i).trim();
      if (tag === "*") return true;
      if (tag && stripWeakEtagPrefix(tag) === current) return true;
      start = i + 1;
    }
  }
  const tag = raw.slice(start).trim();
  if (tag === "*") return true;
  if (tag && stripWeakEtagPrefix(tag) === current) return true;
  return false;
}

function parseHttpDate(value) {
  const millis = Date.parse(value);
  if (!Number.isFinite(millis)) return null;
  return new Date(millis);
}

function ifModifiedSinceMatches(ifModifiedSince, stat) {
  const ims = parseHttpDate(ifModifiedSince);
  if (!ims) return false;
  // HTTP-date has 1-second resolution. Compare at second granularity.
  const resourceSeconds = Math.floor(stat.mtimeMs / 1000);
  const imsSeconds = Math.floor(ims.getTime() / 1000);
  return resourceSeconds <= imsSeconds;
}

function isNotModified(req, stat) {
  const etag = computeEtag(stat);
  const ifNoneMatch = safeHeader(req, "if-none-match");
  if (typeof ifNoneMatch === "string") {
    if (ifNoneMatch.length > MAX_IF_NONE_MATCH_LEN) return false;
    return ifNoneMatchMatches(ifNoneMatch, etag);
  }

  const ifModifiedSince = safeHeader(req, "if-modified-since");
  if (typeof ifModifiedSince === "string") {
    if (ifModifiedSince.length > MAX_IF_MODIFIED_SINCE_LEN) return false;
    return ifModifiedSinceMatches(ifModifiedSince, stat);
  }

  return false;
}

function contentTypeFor(urlPath) {
  if (urlPath.endsWith(".json")) return "application/json";
  return "application/octet-stream";
}

function requireAuth(req) {
  if (typeof args.authToken !== "string" || !args.authToken) return null;
  const auth = safeHeader(req, "authorization");
  if (typeof auth === "string" && auth.length > MAX_AUTH_HEADER_LEN) {
    return { expected: args.authToken, actual: null };
  }
  if (typeof auth !== "string" || auth.trim() !== args.authToken.trim()) {
    return { expected: args.authToken, actual: typeof auth === "string" ? auth : null };
  }
  return null;
}

function cacheControlForRequest(req) {
  if (typeof args.authToken === "string" && args.authToken) {
    // Treat the image as private when auth is enabled. Avoid caching to prevent leaking data via shared caches.
    return "private, no-store, no-transform";
  }
  // Public immutable: assume versioned keys in dev fixtures.
  return "public, max-age=31536000, immutable, no-transform";
}

function setCommonHeaders(req, res, stat, { contentLength, statusCode, urlPath }) {
  if (!setStatusCodeSafe(res, statusCode)) return;
  trySetHeader(res, "Content-Length", String(contentLength));
  trySetHeader(res, "Content-Type", contentTypeFor(urlPath));
  trySetHeader(res, "X-Content-Type-Options", "nosniff");
  trySetHeader(res, "Content-Encoding", "identity");
  trySetHeader(res, "Cache-Control", cacheControlForRequest(req));
  trySetHeader(res, "Last-Modified", stat.mtime.toUTCString());
  trySetHeader(res, "ETag", computeEtag(stat));

  // Defence-in-depth for COEP compatibility: allow the resource to be embedded/fetched cross-origin
  // by default. This is a dev helper; production deployments should choose an appropriate CORP
  // policy (same-site vs cross-origin).
  trySetHeader(res, "Cross-Origin-Resource-Policy", "cross-origin");

  // CORS for browser fetches.
  trySetHeader(res, "Access-Control-Allow-Origin", "*");
  trySetHeader(res, "Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  trySetHeader(
    res,
    "Access-Control-Allow-Headers",
    "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type"
  );
  trySetHeader(
    res,
    "Access-Control-Expose-Headers",
    "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified, Cache-Control, Content-Type"
  );
  trySetHeader(res, "Access-Control-Max-Age", "86400");
  trySetHeader(
    res,
    "Vary",
    "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
  );

  if (args.coopCoep) {
    trySetHeader(res, "Cross-Origin-Opener-Policy", "same-origin");
    trySetHeader(res, "Cross-Origin-Embedder-Policy", "require-corp");
  }
}

function sendRequestError(res, { statusCode, message, method }) {
  const safeMessage = formatOneLineUtf8(message, MAX_ERROR_BODY_BYTES) || "Error";
  const body = method === "HEAD" ? Buffer.alloc(0) : Buffer.from(safeMessage, "utf8");
  trySetHeader(res, "Content-Type", "text/plain; charset=utf-8");
  trySetHeader(res, "Content-Length", String(body.length));
  trySetHeader(res, "Cache-Control", "no-store, no-transform");
  trySetHeader(res, "Content-Encoding", "identity");

  // Defence-in-depth for COEP compatibility.
  trySetHeader(res, "Cross-Origin-Resource-Policy", "cross-origin");
  trySetHeader(res, "Access-Control-Allow-Origin", "*");
  trySetHeader(res, "Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  trySetHeader(
    res,
    "Access-Control-Allow-Headers",
    "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type",
  );
  trySetHeader(
    res,
    "Access-Control-Expose-Headers",
    "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified, Cache-Control, Content-Type",
  );
  trySetHeader(res, "Access-Control-Max-Age", "86400");
  trySetHeader(res, "Vary", "Origin, Access-Control-Request-Method, Access-Control-Request-Headers");

  if (args.coopCoep) {
    trySetHeader(res, "Cross-Origin-Opener-Policy", "same-origin");
    trySetHeader(res, "Cross-Origin-Embedder-Policy", "require-corp");
  }

  finishResponse(res, statusCode, body);
}

function sendAuthError(res, { statusCode, method }) {
  sendRequestError(res, { statusCode, message: "Unauthorized", method });
}

const server = http.createServer((req, res) => {
  const method = safeRequestMethod(req);
  const rawUrl = tryGetProp(req, "url");
  if (typeof rawUrl !== "string" || rawUrl === "") {
    sendRequestError(res, { statusCode: 400, message: "Bad Request", method });
    return;
  }
  if (rawUrl.length > MAX_REQUEST_URL_LEN) {
    sendRequestError(res, { statusCode: 414, message: "URI Too Long", method });
    return;
  }
  if (rawUrl.trim() !== rawUrl) {
    sendRequestError(res, { statusCode: 400, message: "Bad Request", method });
    return;
  }

  if (method === "OPTIONS") {
    // CORS preflight for cross-origin fetches.
    trySetHeader(res, "Cross-Origin-Resource-Policy", "cross-origin");
    trySetHeader(res, "Access-Control-Allow-Origin", "*");
    trySetHeader(res, "Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
    trySetHeader(
      res,
      "Access-Control-Allow-Headers",
      "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type"
    );
    trySetHeader(
      res,
      "Access-Control-Expose-Headers",
      "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified, Cache-Control, Content-Type"
    );
    trySetHeader(res, "Access-Control-Max-Age", "86400");
    trySetHeader(
      res,
      "Vary",
      "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
    );
    if (args.coopCoep) {
      trySetHeader(res, "Cross-Origin-Opener-Policy", "same-origin");
      trySetHeader(res, "Cross-Origin-Embedder-Policy", "require-corp");
    }
    trySetHeader(res, "Content-Encoding", "identity");
    trySetHeader(res, "Cache-Control", "no-store, no-transform");
    trySetHeader(res, "Content-Length", "0");
    finishResponse(res, 204);
    return;
  }

  let url;
  try {
    url = new URL(rawUrl, "http://localhost");
  } catch {
    sendRequestError(res, { statusCode: 400, message: "Bad Request", method });
    return;
  }
  if (url.pathname.length > MAX_PATHNAME_LEN) {
    sendRequestError(res, { statusCode: 414, message: "URI Too Long", method });
    return;
  }
  const filePath = safeJoin(root, url.pathname);
  if (!filePath) {
    sendRequestError(res, { statusCode: 404, message: "Not found", method });
    return;
  }

  fs.stat(filePath, (err, stat) => {
    if (err || !stat.isFile()) {
      sendRequestError(res, { statusCode: 404, message: "Not found", method });
      return;
    }

    const authError = requireAuth(req);
    if (authError) {
      sendAuthError(res, { statusCode: 401, method });
      return;
    }

    if (isNotModified(req, stat)) {
      setCommonHeaders(req, res, stat, { contentLength: 0, statusCode: 304, urlPath: url.pathname });
      finishResponse(res, 304);
      return;
    }

    if (method === "HEAD") {
      setCommonHeaders(req, res, stat, { contentLength: stat.size, statusCode: 200, urlPath: url.pathname });
      finishResponse(res, 200);
      return;
    }

    if (method !== "GET") {
      sendRequestError(res, { statusCode: 405, message: "Method not allowed", method });
      return;
    }

    const stream = fs.createReadStream(filePath);
    setCommonHeaders(req, res, stat, {
      contentLength: stat.size,
      statusCode: 200,
      urlPath: url.pathname,
    });
    void pipeline(stream, res).catch((e) => {
      if (isExpectedStreamAbort(e)) return;
      logServerError("chunk_server: stream error", e);
      clearHeaders(res);
      sendRequestError(res, { statusCode: 500, message: "Internal server error", method });
    });
  });
});

server.listen(args.port, "127.0.0.1", () => {
  console.log(`Serving ${root} on http://127.0.0.1:${args.port}/`);
});
