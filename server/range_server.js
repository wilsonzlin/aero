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
  console.log("Usage: node server/range_server.js --dir <root> --port <port> [--coop-coep]");
  process.exit(0);
}

const root = path.resolve(args.dir);

function safeJoin(rootDir, requestPath) {
  const decoded = decodeURIComponent(requestPath);
  const full = path.resolve(path.join(rootDir, "." + decoded));
  if (!full.startsWith(rootDir + path.sep) && full !== rootDir) return null;
  return full;
}

function sendHeaders(res, stat, { contentLength, contentRange, statusCode }) {
  res.statusCode = statusCode;
  res.setHeader("Accept-Ranges", "bytes");
  res.setHeader("Content-Length", String(contentLength));
  if (contentRange) res.setHeader("Content-Range", contentRange);

  // CORS for Range reads.
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Headers", "Range");
  res.setHeader(
    "Access-Control-Expose-Headers",
    "Accept-Ranges, Content-Range, Content-Length"
  );

  if (args.coopCoep) {
    res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  }

  // Lightweight content-type; raw images are typically `application/octet-stream`.
  res.setHeader("Content-Type", "application/octet-stream");
  res.setHeader("Last-Modified", stat.mtime.toUTCString());
}

function parseRange(rangeHeader, size) {
  // Supports: bytes=start-end (single range only)
  const m = /^bytes=(\d+)-(\d+)$/.exec(rangeHeader);
  if (!m) return null;
  const start = Number(m[1]);
  const endInclusive = Number(m[2]);
  if (!Number.isFinite(start) || !Number.isFinite(endInclusive)) return null;
  if (start >= size) return { error: 416 };
  const endExclusive = Math.min(endInclusive + 1, size);
  if (endExclusive <= start) return { error: 416 };
  return { start, endExclusive };
}

const server = http.createServer((req, res) => {
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

    if (req.method === "HEAD") {
      sendHeaders(res, stat, { contentLength: stat.size, statusCode: 200 });
      res.end();
      return;
    }

    if (req.method !== "GET") {
      res.statusCode = 405;
      res.end("Method not allowed");
      return;
    }

    const rangeHeader = req.headers["range"];
    if (typeof rangeHeader === "string") {
      const parsed = parseRange(rangeHeader, stat.size);
      if (!parsed) {
        res.statusCode = 400;
        res.end("Bad Range");
        return;
      }
      if (parsed.error) {
        res.statusCode = 416;
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
