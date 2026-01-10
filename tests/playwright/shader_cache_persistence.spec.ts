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

test("shader translation is persisted and skipped on next run", async ({}, testInfo) => {
  // This test uses a Chromium persistent context to validate IndexedDB persistence.
  // Skip in other projects to avoid running the same coverage multiple times.
  if (testInfo.project.name !== "chromium") test.skip();

  const rootDir = path.resolve(process.cwd(), "web");
  const server = await startStaticServer(rootDir);

  try {
    // Use a persistent browser profile to ensure IndexedDB survives across browser restarts.
    const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-shader-cache-"));

    async function runOnce(): Promise<{
      cacheHit: boolean;
      translationMs: number;
      telemetry: any;
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

      await page.goto(`${server.baseUrl}/shader_cache_demo.html`);
      await page.waitForFunction(() => (window as any).__shaderCacheDemo?.translationMs !== undefined);

      const result = await page.evaluate(() => (window as any).__shaderCacheDemo);
      await context.close();

      if (result?.error) {
        throw new Error(`demo page failed: ${result.error}`);
      }

      return {
        cacheHit: !!result.cacheHit,
        translationMs: Number(result.translationMs),
        telemetry: result.telemetry,
        logs,
      };
    }

    const first = await runOnce();
    expect(first.cacheHit).toBe(false);
    expect(first.logs.some((l) => l.includes("shader_translate: begin"))).toBe(true);
    expect(first.telemetry?.shader?.misses ?? 0).toBeGreaterThan(0);
    expect(first.telemetry?.shader?.bytesWritten ?? 0).toBeGreaterThan(0);

    const second = await runOnce();
    expect(second.cacheHit).toBe(true);
    expect(second.logs.some((l) => l.includes("shader_translate: begin"))).toBe(false);
    expect(second.telemetry?.shader?.hits ?? 0).toBeGreaterThan(0);
    expect(second.telemetry?.shader?.bytesRead ?? 0).toBeGreaterThan(0);

    // Timing assertion: the simulated translation takes ~300ms on cache miss.
    // Allow a lot of variance for CI load, but ensure the second run is
    // significantly faster.
    expect(first.translationMs).toBeGreaterThan(150);
    expect(second.translationMs).toBeLessThan(first.translationMs / 3);
  } finally {
    await server.close();
  }
});
