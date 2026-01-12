import http from "node:http";
import path from "node:path";
import fs from "node:fs";

function contentTypeForPath(p: string): string {
  if (p.endsWith(".html")) return "text/html; charset=utf-8";
  if (p.endsWith(".js") || p.endsWith(".ts")) return "text/javascript; charset=utf-8";
  if (p.endsWith(".json")) return "application/json; charset=utf-8";
  return "application/octet-stream";
}

export async function startStaticServer(
  rootDir: string,
  opts: { defaultPath?: string } = {},
): Promise<{ baseUrl: string; close: () => Promise<void> }> {
  const defaultPath = opts.defaultPath ?? "/shader_cache_demo.html";

  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    let pathname = decodeURIComponent(url.pathname);
    if (pathname === "/") pathname = defaultPath;

    // `url.pathname` is absolute; remove leading slash before resolving.
    const resolved = path.resolve(rootDir, pathname.slice(1));
    if (!resolved.startsWith(rootDir + path.sep) && resolved !== rootDir) {
      res.writeHead(403).end("Forbidden");
      return;
    }

    fs.readFile(resolved, (err, data) => {
      if (err) {
        res.writeHead(404).end("Not found");
        return;
      }
      res.writeHead(200, { "Content-Type": contentTypeForPath(resolved) });
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

