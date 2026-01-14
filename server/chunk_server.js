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

function parseArgs(argv) {
  const args = { dir: process.cwd(), port: 8080, coopCoep: false };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--dir") args.dir = argv[++i];
    else if (a === "--port") args.port = Number(argv[++i]);
    else if (a === "--coop-coep") args.coopCoep = true;
    else if (a === "--help") args.help = true;
  }
  return args;
}

const args = parseArgs(process.argv);
if (args.help) {
  console.log("Usage: node server/chunk_server.js --dir <root> --port <port> [--coop-coep]");
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
  // HTTP-date has 1-second resolution. Compare at second granularity.
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

function contentTypeFor(urlPath) {
  if (urlPath.endsWith(".json")) return "application/json";
  return "application/octet-stream";
}

function setCommonHeaders(res, stat, { contentLength, statusCode, urlPath }) {
  res.statusCode = statusCode;
  res.setHeader("Content-Length", String(contentLength));
  res.setHeader("Content-Type", contentTypeFor(urlPath));
  res.setHeader("X-Content-Type-Options", "nosniff");
  res.setHeader("Content-Encoding", "identity");
  res.setHeader("Cache-Control", "public, max-age=31536000, immutable, no-transform");
  res.setHeader("Last-Modified", stat.mtime.toUTCString());
  res.setHeader("ETag", computeEtag(stat));

  // Defence-in-depth for COEP compatibility: allow the resource to be embedded/fetched cross-origin
  // by default. This is a dev helper; production deployments should choose an appropriate CORP
  // policy (same-site vs cross-origin).
  res.setHeader("Cross-Origin-Resource-Policy", "cross-origin");

  // CORS for browser fetches.
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  res.setHeader(
    "Access-Control-Allow-Headers",
    "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type"
  );
  res.setHeader(
    "Access-Control-Expose-Headers",
    "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified, Cache-Control, Content-Type"
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
}

const server = http.createServer((req, res) => {
  if (req.method === "OPTIONS") {
    // CORS preflight for cross-origin fetches.
    res.statusCode = 204;
    res.setHeader("Cross-Origin-Resource-Policy", "cross-origin");
    res.setHeader("Access-Control-Allow-Origin", "*");
    res.setHeader("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
    res.setHeader(
      "Access-Control-Allow-Headers",
      "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type"
    );
    res.setHeader(
      "Access-Control-Expose-Headers",
      "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified, Cache-Control, Content-Type"
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
      setCommonHeaders(res, stat, { contentLength: 0, statusCode: 304, urlPath: url.pathname });
      res.end();
      return;
    }

    if (req.method === "HEAD") {
      setCommonHeaders(res, stat, { contentLength: stat.size, statusCode: 200, urlPath: url.pathname });
      res.end();
      return;
    }

    if (req.method !== "GET") {
      res.statusCode = 405;
      res.end("Method not allowed");
      return;
    }

    setCommonHeaders(res, stat, { contentLength: stat.size, statusCode: 200, urlPath: url.pathname });
    fs.createReadStream(filePath).pipe(res);
  });
});

server.listen(args.port, "127.0.0.1", () => {
  console.log(`Serving ${root} on http://127.0.0.1:${args.port}/`);
});

