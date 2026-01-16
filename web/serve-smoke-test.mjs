import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { formatOneLineUtf8 } from "../src/text.js";

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

function safeResolve(rootDir, requestPath) {
  const rootResolved = resolve(rootDir);
  const resolved = resolve(rootResolved, `.${requestPath}`);
  if (!resolved.startsWith(rootResolved + sep) && resolved !== rootResolved) return null;
  return resolved;
}

const server = createServer(async (req, res) => {
  try {
    const rawUrl = req.url ?? "/";
    if (typeof rawUrl !== "string") {
      res.writeHead(400);
      res.end(safeTextBody("Bad Request"));
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      res.writeHead(414);
      res.end(safeTextBody("URI Too Long"));
      return;
    }

    let url;
    try {
      url = new URL(rawUrl, "http://localhost");
    } catch {
      res.writeHead(400);
      res.end(safeTextBody("Bad Request"));
      return;
    }
    if (url.pathname.length > MAX_PATHNAME_LEN) {
      res.writeHead(414);
      res.end(safeTextBody("URI Too Long"));
      return;
    }
    let pathname;
    try {
      pathname = decodeURIComponent(url.pathname);
    } catch {
      res.writeHead(400);
      res.end(safeTextBody("Bad Request"));
      return;
    }
    if (pathname.length > MAX_PATHNAME_LEN) {
      res.writeHead(414);
      res.end(safeTextBody("URI Too Long"));
      return;
    }
    if (pathname.includes("\0")) {
      res.writeHead(400);
      res.end(safeTextBody("Bad Request"));
      return;
    }

    if (pathname === "/") pathname = "/virtio-snd-smoke-test.html";
    const abs = safeResolve(webRoot, pathname);
    if (!abs) {
      res.writeHead(404);
      res.end(safeTextBody("Not found"));
      return;
    }

    const data = await readFile(abs);
    res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
    res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
    res.setHeader(
      "Content-Type",
      mimeTypes[extname(abs)] ?? "application/octet-stream",
    );
    res.writeHead(200);
    res.end(data);
  } catch {
    res.writeHead(404);
    res.end(safeTextBody("Not found"));
  }
});

const port = Number(process.env.PORT ?? 8000);
server.listen(port, () => {
  // eslint-disable-next-line no-console
  console.log(`Serving ${webRoot} on http://localhost:${port}/`);
});
