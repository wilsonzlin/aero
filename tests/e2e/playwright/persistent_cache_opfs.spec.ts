import { test, expect, chromium } from "@playwright/test";
import http from "node:http";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";

function contentTypeForPath(p: string): string {
  if (p.endsWith(".html")) return "text/html; charset=utf-8";
  if (p.endsWith(".js") || p.endsWith(".ts")) return "text/javascript; charset=utf-8";
  if (p.endsWith(".json")) return "application/json; charset=utf-8";
  return "application/octet-stream";
}

async function startStaticServer(rootDir: string): Promise<{ baseUrl: string; close: () => Promise<void> }> {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    let pathname = decodeURIComponent(url.pathname);
    if (pathname === "/") pathname = "/shader_cache_demo.html";

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

test("large shader payload spills to OPFS when available", async ({}, testInfo) => {
  // Avoid redundant coverage in other projects.
  if (testInfo.project.name !== "chromium") test.skip();

  const rootDir = path.resolve(process.cwd(), "web");
  const server = await startStaticServer(rootDir);

  try {
    const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-shader-cache-opfs-"));

    async function runOnce(): Promise<{
      key: string;
      cacheHit: boolean;
      opfsAvailable: boolean;
      opfsFileExists: boolean;
      idbShaderRecord: { storage: string | null; opfsFile: string | null; hasWgsl: boolean } | null;
      logs: string[];
    }> {
      const logs: string[] = [];
      const context = await chromium.launchPersistentContext(userDataDir, {
        headless: true,
        // WebGPU may be behind a flag in some Chromium configurations.
        args: ["--enable-unsafe-webgpu"],
      });
      const page = await context.newPage();
      page.on("console", (msg) => logs.push(msg.text()));

      await page.goto(`${server.baseUrl}/shader_cache_demo.html?large=1`);
      await page.waitForFunction(() => (window as any).__shaderCacheDemo !== undefined);

      const result = await page.evaluate(() => (window as any).__shaderCacheDemo);
      if (result?.error) {
        await context.close();
        throw new Error(`demo page failed: ${result.error}\nlogs:\n${logs.join("\n")}`);
      }

      const idbShaderRecord = await page.evaluate(async (key: string) => {
        const DB_NAME = "aero-gpu-cache";
        const STORE_SHADERS = "shaders";

        const db = await new Promise<IDBDatabase>((resolve, reject) => {
          // Open without forcing a schema version so the test stays compatible
          // if the cache bumps `DB_VERSION` in the future.
          const req = indexedDB.open(DB_NAME);
          req.onerror = () => reject(req.error ?? new Error("IndexedDB open failed"));
          req.onsuccess = () => resolve(req.result);
        });

        try {
          const tx = db.transaction([STORE_SHADERS], "readonly");
          const store = tx.objectStore(STORE_SHADERS);
          const record = await new Promise<any>((resolve, reject) => {
            const req = store.get(key);
            req.onerror = () => reject(req.error ?? new Error("IndexedDB get failed"));
            req.onsuccess = () => resolve(req.result ?? null);
          });
          await new Promise<void>((resolve) => {
            tx.oncomplete = () => resolve();
            tx.onabort = () => resolve(); // best-effort; return what we have
            tx.onerror = () => resolve();
          });

          if (!record) return null;
          return {
            storage: typeof record.storage === "string" ? record.storage : null,
            opfsFile: typeof record.opfsFile === "string" ? record.opfsFile : null,
            hasWgsl: typeof record.wgsl === "string",
          };
        } finally {
          db.close();
        }
      }, result.key);

      await context.close();

      return {
        key: String(result.key),
        cacheHit: !!result.cacheHit,
        opfsAvailable: !!result.opfsAvailable,
        opfsFileExists: !!result.opfsFileExists,
        idbShaderRecord: idbShaderRecord ?? null,
        logs,
      };
    }

    const first = await runOnce();
    if (!first.opfsAvailable) {
      test.skip(true, "OPFS not available in this Chromium configuration");
    }
    expect(first.cacheHit).toBe(false);
    expect(first.opfsFileExists).toBe(true);
    expect(first.logs.some((l) => l.includes("shader_translate: begin"))).toBe(true);
    expect(first.idbShaderRecord?.storage).toBe("opfs");
    expect(first.idbShaderRecord?.opfsFile).toBe(`${first.key}.json`);
    expect(first.idbShaderRecord?.hasWgsl).toBe(false);

    const second = await runOnce();
    expect(second.key).toBe(first.key);
    expect(second.cacheHit).toBe(true);
    expect(second.opfsFileExists).toBe(true);
    expect(second.logs.some((l) => l.includes("shader_translate: begin"))).toBe(false);
    expect(second.idbShaderRecord?.storage).toBe("opfs");
  } finally {
    await server.close();
  }
});
