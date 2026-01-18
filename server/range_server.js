#!/usr/bin/env node
/**
 * Minimal static file server with HTTP Range + CORS headers.
 *
 * Intended for development/testing of Aero's streaming disk backend.
 *
 * Example:
 *   node server/range_server.js --dir ./images --port 8080 --coop-coep
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
const MAX_RANGE_HEADER_LEN = 16 * 1024;
const MAX_IF_NONE_MATCH_LEN = 16 * 1024;
const MAX_IF_MODIFIED_SINCE_LEN = 128;
const MAX_IF_RANGE_LEN = 256;
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
    "Usage: node server/range_server.js --dir <root> --port <port> [--coop-coep] [--auth-token <Authorization-value>]"
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
  // HTTP-date has 1-second resolution. Compare at second granularity to avoid false negatives when
  // the filesystem provides sub-second mtimes.
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

function ifRangeAllowsRange(req, stat) {
  const ifRange = safeHeader(req, "if-range");
  if (typeof ifRange !== "string") return true;
  if (ifRange.length > MAX_IF_RANGE_LEN) return false;

  const value = ifRange.trim();
  if (!value) return false;

  // Entity-tag form. RFC 9110 requires strong comparison and disallows weak validators.
  if (value.startsWith('"') || /^w\//i.test(value)) {
    if (/^w\//i.test(value)) return false;
    return value === computeEtag(stat);
  }

  // HTTP-date form.
  const since = parseHttpDate(value);
  if (!since) return false;
  const resourceSeconds = Math.floor(stat.mtimeMs / 1000);
  const sinceSeconds = Math.floor(since.getTime() / 1000);
  return resourceSeconds <= sinceSeconds;
}

function sendHeaders(res, stat, { contentLength, contentRange, statusCode }) {
  if (!setStatusCodeSafe(res, statusCode)) return;
  trySetHeader(res, "Accept-Ranges", "bytes");
  trySetHeader(res, "Content-Length", String(contentLength));
  if (contentRange) trySetHeader(res, "Content-Range", contentRange);

  // Defence-in-depth for COEP compatibility: allow the resource to be embedded/fetched cross-origin
  // by default. This is a dev helper; production deployments should choose an appropriate CORP
  // policy (same-site vs cross-origin).
  trySetHeader(res, "Cross-Origin-Resource-Policy", "cross-origin");

  // CORS for Range reads.
  trySetHeader(res, "Access-Control-Allow-Origin", "*");
  trySetHeader(
    res,
    "Access-Control-Allow-Headers",
    "Range, If-Range, If-None-Match, If-Modified-Since"
  );
  trySetHeader(res, "Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  trySetHeader(
    res,
    "Access-Control-Expose-Headers",
    "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified"
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

  // Lightweight content-type; raw images are typically `application/octet-stream`.
  trySetHeader(res, "Content-Type", "application/octet-stream");
  trySetHeader(res, "X-Content-Type-Options", "nosniff");
  trySetHeader(res, "Content-Encoding", "identity");
  trySetHeader(
    res,
    "Cache-Control",
    args.authToken ? "private, no-store, no-transform" : "no-transform"
  );
  trySetHeader(res, "Last-Modified", stat.mtime.toUTCString());
  trySetHeader(res, "ETag", computeEtag(stat));
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

function sendRequestError(req, res, { statusCode, message }) {
  const safeMessage = formatOneLineUtf8(message, MAX_ERROR_BODY_BYTES) || "Error";
  const body = safeRequestMethod(req) === "HEAD" ? Buffer.alloc(0) : Buffer.from(safeMessage, "utf8");
  trySetHeader(res, "Accept-Ranges", "bytes");
  trySetHeader(res, "Content-Type", "text/plain; charset=utf-8");
  trySetHeader(res, "Content-Length", String(body.length));
  trySetHeader(res, "Content-Encoding", "identity");
  trySetHeader(
    res,
    "Cache-Control",
    args.authToken ? "private, no-store, no-transform" : "no-transform",
  );

  trySetHeader(res, "Cross-Origin-Resource-Policy", "cross-origin");
  trySetHeader(res, "Access-Control-Allow-Origin", "*");
  trySetHeader(
    res,
    "Access-Control-Allow-Headers",
    "Range, If-Range, If-None-Match, If-Modified-Since, Authorization",
  );
  trySetHeader(res, "Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  trySetHeader(
    res,
    "Access-Control-Expose-Headers",
    "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified",
  );
  trySetHeader(res, "Access-Control-Max-Age", "86400");
  trySetHeader(res, "Vary", "Origin, Access-Control-Request-Method, Access-Control-Request-Headers");

  if (args.coopCoep) {
    trySetHeader(res, "Cross-Origin-Opener-Policy", "same-origin");
    trySetHeader(res, "Cross-Origin-Embedder-Policy", "require-corp");
  }

  finishResponse(res, statusCode, body);
}

function sendAuthError(req, res, { statusCode }) {
  sendRequestError(req, res, { statusCode, message: "Unauthorized" });
}

function parseRange(rangeHeader, size) {
  // Supports single byte ranges only:
  // - bytes=start-end
  // - bytes=start-
  // - bytes=-suffixLen
  const trimmed = rangeHeader.trim();
  const parts = trimmed.split("=");
  if (parts.length !== 2) return null;
  const unit = parts[0].trim().toLowerCase();
  if (unit !== "bytes") return { ignore: true };
  const spec = parts[1].trim();
  if (!spec || spec.includes(",")) return null;

  if (spec.startsWith("-")) {
    const len = Number(spec.slice(1).trim());
    if (!Number.isFinite(len) || !Number.isInteger(len) || len <= 0) return null;
    const suffix = Math.min(len, size);
    const start = size - suffix;
    return { start, endExclusive: size };
  }

  const dash = spec.indexOf("-");
  if (dash === -1) return null;
  const startStr = spec.slice(0, dash).trim();
  const endStr = spec.slice(dash + 1).trim();
  const start = Number(startStr);
  if (!Number.isFinite(start) || !Number.isInteger(start) || start < 0) return null;
  if (start >= size) return { error: 416 };

  if (!endStr) {
    return { start, endExclusive: size };
  }

  const endInclusive = Number(endStr);
  if (!Number.isFinite(endInclusive) || !Number.isInteger(endInclusive) || endInclusive < 0)
    return null;
  if (endInclusive < start) return null;
  const endExclusive = Math.min(endInclusive + 1, size);
  if (endExclusive <= start) return { error: 416 };
  return { start, endExclusive };
}

const server = http.createServer((req, res) => {
  const method = safeRequestMethod(req);
  const rawUrl = tryGetProp(req, "url");
  if (typeof rawUrl !== "string" || rawUrl === "") {
    sendRequestError(req, res, { statusCode: 400, message: "Bad Request" });
    return;
  }
  if (rawUrl.length > MAX_REQUEST_URL_LEN) {
    sendRequestError(req, res, { statusCode: 414, message: "URI Too Long" });
    return;
  }
  if (rawUrl.trim() !== rawUrl) {
    sendRequestError(req, res, { statusCode: 400, message: "Bad Request" });
    return;
  }

  if (method === "OPTIONS") {
    // CORS preflight for cross-origin Range requests.
    trySetHeader(res, "Cross-Origin-Resource-Policy", "cross-origin");
    trySetHeader(res, "Access-Control-Allow-Origin", "*");
    trySetHeader(res, "Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
    trySetHeader(
      res,
      "Access-Control-Allow-Headers",
      "Range, If-Range, If-None-Match, If-Modified-Since, Authorization"
    );
    trySetHeader(
      res,
      "Access-Control-Expose-Headers",
      "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified"
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

  const authError = requireAuth(req);
  if (authError) {
    sendAuthError(req, res, { statusCode: 401 });
    return;
  }

  let url;
  try {
    url = new URL(rawUrl, "http://localhost");
  } catch {
    sendRequestError(req, res, { statusCode: 400, message: "Bad Request" });
    return;
  }
  if (url.pathname.length > MAX_PATHNAME_LEN) {
    sendRequestError(req, res, { statusCode: 414, message: "URI Too Long" });
    return;
  }
  const filePath = safeJoin(root, url.pathname);
  if (!filePath) {
    sendRequestError(req, res, { statusCode: 404, message: "Not found" });
    return;
  }

  fs.stat(filePath, (err, stat) => {
    if (err || !stat.isFile()) {
      sendRequestError(req, res, { statusCode: 404, message: "Not found" });
      return;
    }

    if (isNotModified(req, stat)) {
      sendHeaders(res, stat, { contentLength: 0, statusCode: 304 });
      finishResponse(res, 304);
      return;
    }

    if (method === "HEAD") {
      const rangeHeader = safeHeader(req, "range");
      const ifRangeOk = ifRangeAllowsRange(req, stat);
      if (typeof rangeHeader === "string" && ifRangeOk) {
        if (rangeHeader.length > MAX_RANGE_HEADER_LEN) {
          sendRequestError(req, res, { statusCode: 413, message: "Range header too large" });
          return;
        }
        const parsed = parseRange(rangeHeader, stat.size);
        if (parsed && parsed.ignore) {
          // Ignore unknown Range unit.
        } else if (!parsed || parsed.error) {
          sendHeaders(res, stat, {
            statusCode: 416,
            contentLength: 0,
            contentRange: `bytes */${stat.size}`,
          });
          finishResponse(res, 416);
          return;
        } else {
          const { start, endExclusive } = parsed;
          const endInclusive = endExclusive - 1;
          sendHeaders(res, stat, {
            statusCode: 206,
            contentLength: endExclusive - start,
            contentRange: `bytes ${start}-${endInclusive}/${stat.size}`,
          });
          finishResponse(res, 206);
          return;
        }
      }

      sendHeaders(res, stat, { contentLength: stat.size, statusCode: 200 });
      finishResponse(res, 200);
      return;
    }

    if (method !== "GET") {
      sendRequestError(req, res, { statusCode: 405, message: "Method not allowed" });
      return;
    }

    let rangeHeader = safeHeader(req, "range");
    if (typeof rangeHeader === "string") {
      if (rangeHeader.length > MAX_RANGE_HEADER_LEN) {
        sendRequestError(req, res, { statusCode: 413, message: "Range header too large" });
        return;
      }
      if (!ifRangeAllowsRange(req, stat)) {
        rangeHeader = undefined;
      }
    }

    if (typeof rangeHeader === "string") {
      const parsed = parseRange(rangeHeader, stat.size);
      if (parsed && parsed.ignore) {
        // Ignore unknown Range unit.
        rangeHeader = undefined;
      }
    }

    if (typeof rangeHeader === "string") {
      const parsed = parseRange(rangeHeader, stat.size);
      if (!parsed || parsed.error) {
        // For unsatisfiable/invalid ranges, return 416 + Content-Range bytes */<size>.
        sendHeaders(res, stat, {
          statusCode: 416,
          contentLength: 0,
          contentRange: `bytes */${stat.size}`,
        });
        finishResponse(res, 416);
        return;
      }

      const { start, endExclusive } = parsed;
      const endInclusive = endExclusive - 1;
      sendHeaders(res, stat, {
        statusCode: 206,
        contentLength: endExclusive - start,
        contentRange: `bytes ${start}-${endInclusive}/${stat.size}`,
      });
      const stream = fs.createReadStream(filePath, { start, end: endInclusive });
      void pipeline(stream, res).catch((e) => {
        if (isExpectedStreamAbort(e)) return;
        logServerError("range_server: stream error", e);
        clearHeaders(res);
        sendRequestError(req, res, { statusCode: 500, message: "Internal server error" });
      });
      return;
    }

    sendHeaders(res, stat, { contentLength: stat.size, statusCode: 200 });
    const stream = fs.createReadStream(filePath);
    void pipeline(stream, res).catch((e) => {
      if (isExpectedStreamAbort(e)) return;
      logServerError("range_server: stream error", e);
      clearHeaders(res);
      sendRequestError(req, res, { statusCode: 500, message: "Internal server error" });
    });
  });
});

server.listen(args.port, "127.0.0.1", () => {
  console.log(`Serving ${root} on http://127.0.0.1:${args.port}/`);
});
