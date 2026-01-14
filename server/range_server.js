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

function splitCommaHeaderOutsideQuotes(value) {
  const out = [];
  let start = 0;
  let inQuotes = false;
  let escaped = false;
  for (let i = 0; i < value.length; i++) {
    const ch = value[i];
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
      out.push(value.slice(start, i));
      start = i + 1;
    }
  }
  out.push(value.slice(start));
  return out;
}

function ifNoneMatchMatches(ifNoneMatch, currentEtag) {
  const raw = String(ifNoneMatch).trim();
  if (!raw) return false;
  if (raw === "*") return true;

  const current = stripWeakEtagPrefix(currentEtag);
  for (const part of splitCommaHeaderOutsideQuotes(raw)) {
    const candidate = part.trim();
    if (!candidate) continue;
    if (candidate === "*") return true;
    if (stripWeakEtagPrefix(candidate) === current) return true;
  }
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
  const ifNoneMatch = req.headers["if-none-match"];
  if (typeof ifNoneMatch === "string") {
    return ifNoneMatchMatches(ifNoneMatch, etag);
  }

  const ifModifiedSince = req.headers["if-modified-since"];
  if (typeof ifModifiedSince === "string") {
    return ifModifiedSinceMatches(ifModifiedSince, stat);
  }

  return false;
}

function ifRangeAllowsRange(req, stat) {
  const ifRange = req.headers["if-range"];
  if (typeof ifRange !== "string") return true;

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
  res.statusCode = statusCode;
  res.setHeader("Accept-Ranges", "bytes");
  res.setHeader("Content-Length", String(contentLength));
  if (contentRange) res.setHeader("Content-Range", contentRange);

  // Defence-in-depth for COEP compatibility: allow the resource to be embedded/fetched cross-origin
  // by default. This is a dev helper; production deployments should choose an appropriate CORP
  // policy (same-site vs cross-origin).
  res.setHeader("Cross-Origin-Resource-Policy", "cross-origin");

  // CORS for Range reads.
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader(
    "Access-Control-Allow-Headers",
    "Range, If-Range, If-None-Match, If-Modified-Since"
  );
  res.setHeader("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  res.setHeader(
    "Access-Control-Expose-Headers",
    "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified"
  );
  res.setHeader("Access-Control-Max-Age", "86400");
  res.setHeader(
    "Vary",
    "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
  );

  if (args.coopCoep) {
    res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  }

  // Lightweight content-type; raw images are typically `application/octet-stream`.
  res.setHeader("Content-Type", "application/octet-stream");
  res.setHeader("X-Content-Type-Options", "nosniff");
  res.setHeader("Content-Encoding", "identity");
  res.setHeader(
    "Cache-Control",
    args.authToken ? "private, no-store, no-transform" : "no-transform"
  );
  res.setHeader("Last-Modified", stat.mtime.toUTCString());
  res.setHeader("ETag", computeEtag(stat));
}

function requireAuth(req) {
  if (typeof args.authToken !== "string" || !args.authToken) return null;
  const auth = req.headers["authorization"];
  if (typeof auth !== "string" || auth.trim() !== args.authToken.trim()) {
    return { expected: args.authToken, actual: typeof auth === "string" ? auth : null };
  }
  return null;
}

function sendAuthError(req, res, { statusCode }) {
  const body = req.method === "HEAD" ? Buffer.alloc(0) : Buffer.from("Unauthorized");
  res.statusCode = statusCode;
  res.setHeader("Accept-Ranges", "bytes");
  res.setHeader("Content-Type", "text/plain; charset=utf-8");
  res.setHeader("Content-Length", String(body.length));
  res.setHeader("Content-Encoding", "identity");
  res.setHeader("Cache-Control", "private, no-store, no-transform");

  res.setHeader("Cross-Origin-Resource-Policy", "cross-origin");
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader(
    "Access-Control-Allow-Headers",
    "Range, If-Range, If-None-Match, If-Modified-Since, Authorization"
  );
  res.setHeader("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  res.setHeader(
    "Access-Control-Expose-Headers",
    "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified"
  );
  res.setHeader("Access-Control-Max-Age", "86400");
  res.setHeader(
    "Vary",
    "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
  );

  if (args.coopCoep) {
    res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  }

  res.end(body);
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
  if (req.method === "OPTIONS") {
    // CORS preflight for cross-origin Range requests.
    res.statusCode = 204;
    res.setHeader("Cross-Origin-Resource-Policy", "cross-origin");
    res.setHeader("Access-Control-Allow-Origin", "*");
    res.setHeader("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
    res.setHeader(
      "Access-Control-Allow-Headers",
      "Range, If-Range, If-None-Match, If-Modified-Since, Authorization"
    );
    res.setHeader(
      "Access-Control-Expose-Headers",
      "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified"
    );
    res.setHeader("Access-Control-Max-Age", "86400");
    res.setHeader(
      "Vary",
      "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
    );
    if (args.coopCoep) {
      res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
      res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
    }
    res.end();
    return;
  }

  const authError = requireAuth(req);
  if (authError) {
    sendAuthError(req, res, { statusCode: 401 });
    return;
  }

  const url = new URL(req.url ?? "/", "http://localhost");
  const filePath = safeJoin(root, url.pathname);
  if (!filePath) {
    res.statusCode = 404;
    res.end("Not found");
    return;
  }

  fs.stat(filePath, (err, stat) => {
    if (err || !stat.isFile()) {
      res.statusCode = 404;
      res.end("Not found");
      return;
    }

    if (isNotModified(req, stat)) {
      sendHeaders(res, stat, { contentLength: 0, statusCode: 304 });
      res.end();
      return;
    }

    if (req.method === "HEAD") {
      const rangeHeader = req.headers["range"];
      const ifRangeOk = ifRangeAllowsRange(req, stat);
      if (typeof rangeHeader === "string" && ifRangeOk) {
        const parsed = parseRange(rangeHeader, stat.size);
        if (parsed && parsed.ignore) {
          // Ignore unknown Range unit.
        } else if (!parsed || parsed.error) {
          sendHeaders(res, stat, {
            statusCode: 416,
            contentLength: 0,
            contentRange: `bytes */${stat.size}`,
          });
          res.end();
          return;
        } else {
          const { start, endExclusive } = parsed;
          const endInclusive = endExclusive - 1;
          sendHeaders(res, stat, {
            statusCode: 206,
            contentLength: endExclusive - start,
            contentRange: `bytes ${start}-${endInclusive}/${stat.size}`,
          });
          res.end();
          return;
        }
      }

      sendHeaders(res, stat, { contentLength: stat.size, statusCode: 200 });
      res.end();
      return;
    }

    if (req.method !== "GET") {
      res.statusCode = 405;
      res.end("Method not allowed");
      return;
    }

    let rangeHeader = req.headers["range"];
    if (typeof rangeHeader === "string") {
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
        res.end();
        return;
      }

      const { start, endExclusive } = parsed;
      const endInclusive = endExclusive - 1;
      sendHeaders(res, stat, {
        statusCode: 206,
        contentLength: endExclusive - start,
        contentRange: `bytes ${start}-${endInclusive}/${stat.size}`,
      });
      fs.createReadStream(filePath, { start, end: endInclusive }).pipe(res);
      return;
    }

    sendHeaders(res, stat, { contentLength: stat.size, statusCode: 200 });
    fs.createReadStream(filePath).pipe(res);
  });
});

server.listen(args.port, "127.0.0.1", () => {
  console.log(`Serving ${root} on http://127.0.0.1:${args.port}/`);
});
