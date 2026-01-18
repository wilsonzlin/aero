import http from "node:http";
import path from "node:path";
import fs from "node:fs";

import { sendEmpty, sendText } from "./helpers/http_test_response.js";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;

function contentTypeForPath(p: string): string {
  if (p.endsWith(".html")) return "text/html; charset=utf-8";
  if (p.endsWith(".js") || p.endsWith(".ts")) return "text/javascript; charset=utf-8";
  if (p.endsWith(".json")) return "application/json; charset=utf-8";
  return "application/octet-stream";
}

/**
 * Lightweight static server for Playwright e2e tests.
 *
 * Note: this deliberately avoids Vite to keep CI startup cost low, but it still needs to serve
 * TypeScript modules directly in the browser.
 */
export async function startStaticServer(
  rootDir: string,
  opts: { defaultPath?: string } = {},
): Promise<{ baseUrl: string; close: () => Promise<void> }> {
  const defaultPath = opts.defaultPath ?? "/shader_cache_demo.html";

  const server = http.createServer((req, res) => {
    const method = req.method ?? "GET";
    const allow = "GET, HEAD, OPTIONS";
    if (method === "OPTIONS") {
      sendEmpty(res, 204, { allow });
      return;
    }
    if (method !== "GET" && method !== "HEAD") {
      sendText(res, 405, "Method Not Allowed", { allow });
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

    let url: URL;
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

    let pathname: string;
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
    if (pathname === "/") pathname = defaultPath;

    const rootResolved = path.resolve(rootDir);
    const resolved = path.resolve(rootResolved, `.${pathname}`);
    if (!resolved.startsWith(rootResolved + path.sep) && resolved !== rootResolved) {
      sendText(res, 403, "Forbidden");
      return;
    }

    fs.readFile(resolved, (err, data) => {
      if (err) {
        sendText(res, 404, "Not found");
        return;
      }
      res.statusCode = 200;
      res.setHeader("Content-Type", contentTypeForPath(resolved));
      res.setHeader("Content-Length", String(data.byteLength));
      if (method === "HEAD") {
        res.end();
        return;
      }
      res.end(data);
    });
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", () => resolve()));
  const addr = server.address();
  if (!addr || typeof addr === "string") throw new Error("Failed to listen on server");

  return {
    baseUrl: `http://127.0.0.1:${addr.port}`,
    close: async () => {
      await new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
    },
  };
}

