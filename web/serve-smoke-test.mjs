import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = resolve(fileURLToPath(new URL(".", import.meta.url)));

const mimeTypes = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
};

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url ?? "/", "http://localhost");
    let path = url.pathname === "/" ? "/virtio-snd-smoke-test.html" : url.pathname;
    path = normalize(path).replace(/^(\.\.(\/|\\|$))+/, "");
    const abs = resolve(join(webRoot, path));
    if (!abs.startsWith(webRoot)) {
      res.writeHead(404);
      res.end("Not found");
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
    res.end("Not found");
  }
});

const port = Number(process.env.PORT ?? 8000);
server.listen(port, () => {
  // eslint-disable-next-line no-console
  console.log(`Serving ${webRoot} on http://localhost:${port}/`);
});
