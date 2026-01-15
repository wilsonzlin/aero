import http from "node:http";
import { promises as fs } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "../..");

const port = Number(process.env.PORT ?? 8000);

function contentTypeFor(filePath) {
  if (filePath.endsWith(".html")) return "text/html; charset=utf-8";
  if (filePath.endsWith(".js")) return "text/javascript; charset=utf-8";
  if (filePath.endsWith(".css")) return "text/css; charset=utf-8";
  if (filePath.endsWith(".json")) return "application/json; charset=utf-8";
  return "application/octet-stream";
}

function withSABHeaders(res) {
  // Required for SharedArrayBuffer in browsers.
  res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
  res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  // Convenient for local files fetched by demo.
  res.setHeader("Cross-Origin-Resource-Policy", "cross-origin");
}

const server = http.createServer(async (req, res) => {
  try {
    withSABHeaders(res);

    const rawUrl = req.url ?? "/";
    if (typeof rawUrl !== "string") {
      res.writeHead(400);
      res.end("Bad Request");
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      res.writeHead(414);
      res.end("URI Too Long");
      return;
    }

    const url = new URL(rawUrl, "http://localhost");
    if (url.pathname.length > MAX_PATHNAME_LEN) {
      res.writeHead(414);
      res.end("URI Too Long");
      return;
    }

    let decodedPath;
    try {
      decodedPath = decodeURIComponent(url.pathname);
    } catch {
      res.writeHead(400);
      res.end("Bad Request");
      return;
    }

    // Serve from repo root, but prevent directory traversal.
    let filePath = path.normalize(path.join(repoRoot, decodedPath));
    if (!filePath.startsWith(repoRoot)) {
      res.writeHead(403);
      res.end("Forbidden");
      return;
    }

    const stat = await fs.stat(filePath).catch(() => null);
    if (stat?.isDirectory()) {
      filePath = path.join(filePath, "index.html");
    }

    const data = await fs.readFile(filePath);
    res.writeHead(200, { "Content-Type": contentTypeFor(filePath) });
    res.end(data);
  } catch (err) {
    res.writeHead(404);
    res.end("Not Found");
  }
});

server.listen(port, () => {
  // eslint-disable-next-line no-console
  console.log(`Aero perf demo server running at http://localhost:${port}/web/demo/`);
});

