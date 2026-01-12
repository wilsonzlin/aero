import { test, expect, chromium } from "@playwright/test";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";

import { startStaticServer } from "./utils/static_server";

test("large shader payload spills to OPFS when available", async ({}, testInfo) => {
  // Avoid redundant coverage in other projects.
  if (testInfo.project.name !== "chromium") test.skip();

  const rootDir = path.resolve(process.cwd(), "web");
  const server = await startStaticServer(rootDir);

  let userDataDir: string | null = null;
  try {
    userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-shader-cache-opfs-"));

    async function runOnce(): Promise<{
      key: string;
      cacheHit: boolean;
      opfsAvailable: boolean;
      opfsFileExists: boolean;
      opfsFileSize: number | null;
      opfsWgslBytes: number | null;
      idbShaderRecord: {
        storage: string | null;
        opfsFile: string | null;
        hasWgsl: boolean;
        hasReflection: boolean;
        size: number | null;
      } | null;
      logs: string[];
    }> {
      const logs: string[] = [];
      const context = await chromium.launchPersistentContext(userDataDir, {
        headless: true,
        // WebGPU may be behind a flag in some Chromium configurations.
        args: ["--enable-unsafe-webgpu"],
      });
      try {
        const page = await context.newPage();
        page.on("console", (msg) => logs.push(msg.text()));

        await page.goto(`${server.baseUrl}/shader_cache_demo.html?large=1`);
        await page.waitForFunction(() => (window as any).__shaderCacheDemo !== undefined);

        const result = await page.evaluate(() => (window as any).__shaderCacheDemo);
        if (result?.error) {
          throw new Error(`demo page failed: ${result.error}`);
        }

        const opfsInfo = await page.evaluate(async (key: string) => {
          if (!navigator.storage || typeof navigator.storage.getDirectory !== "function") {
            return { fileSize: null, wgslBytes: null };
          }

          try {
            const root = await navigator.storage.getDirectory();
            const dir = await root.getDirectoryHandle("aero-gpu-cache", { create: true });
            const shadersDir = await dir.getDirectoryHandle("shaders");
            const handle = await shadersDir.getFileHandle(`${key}.json`);
            const file = await handle.getFile();
            const text = await file.text();

            let wgslBytes: number | null = null;
            try {
              const parsed = JSON.parse(text);
              if (parsed && typeof parsed.wgsl === "string") {
                wgslBytes = new TextEncoder().encode(parsed.wgsl).byteLength;
              }
            } catch {
              wgslBytes = null;
            }

            return { fileSize: typeof file.size === "number" ? file.size : null, wgslBytes };
          } catch {
            return { fileSize: null, wgslBytes: null };
          }
        }, result.key);

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
              hasReflection: record.reflection !== undefined,
              size: typeof record.size === "number" && Number.isFinite(record.size) ? record.size : null,
            };
          } finally {
            db.close();
          }
        }, result.key);

        return {
          key: String(result.key),
          cacheHit: !!result.cacheHit,
          opfsAvailable: !!result.opfsAvailable,
          opfsFileExists: !!result.opfsFileExists,
          opfsFileSize: opfsInfo.fileSize ?? null,
          opfsWgslBytes: opfsInfo.wgslBytes ?? null,
          idbShaderRecord: idbShaderRecord ?? null,
          logs,
        };
      } catch (err) {
        // Include browser logs to make failures actionable in CI.
        throw new Error(`${String(err)}\nlogs:\n${logs.join("\n")}`);
      } finally {
        try {
          await context.close();
        } catch {
          // Ignore.
        }
      }
    }

    const first = await runOnce();
    if (!first.opfsAvailable) {
      test.skip(true, "OPFS not available in this Chromium configuration");
    }
    expect(first.cacheHit).toBe(false);
    expect(first.opfsFileExists).toBe(true);
    expect(first.opfsFileSize).not.toBeNull();
    expect(first.opfsFileSize ?? 0).toBeGreaterThan(256 * 1024);
    expect(first.opfsWgslBytes).not.toBeNull();
    expect(first.opfsWgslBytes ?? 0).toBeGreaterThan(300 * 1024);
    expect(first.logs.some((l) => l.includes("shader_translate: begin"))).toBe(true);
    expect(first.idbShaderRecord?.storage).toBe("opfs");
    expect(first.idbShaderRecord?.opfsFile).toBe(`${first.key}.json`);
    expect(first.idbShaderRecord?.hasWgsl).toBe(false);
    expect(first.idbShaderRecord?.hasReflection).toBe(false);
    expect(first.idbShaderRecord?.size).not.toBeNull();
    expect(first.idbShaderRecord?.size ?? 0).toBeGreaterThan(256 * 1024);

    const second = await runOnce();
    expect(second.key).toBe(first.key);
    expect(second.cacheHit).toBe(true);
    expect(second.opfsFileExists).toBe(true);
    expect(second.opfsFileSize).not.toBeNull();
    expect(second.opfsFileSize ?? 0).toBeGreaterThan(256 * 1024);
    expect(second.opfsWgslBytes).not.toBeNull();
    expect(second.opfsWgslBytes ?? 0).toBeGreaterThan(300 * 1024);
    expect(second.logs.some((l) => l.includes("shader_translate: begin"))).toBe(false);
    expect(second.idbShaderRecord?.storage).toBe("opfs");
    expect(second.idbShaderRecord?.hasWgsl).toBe(false);
    expect(second.idbShaderRecord?.hasReflection).toBe(false);
  } finally {
    await server.close();
    if (userDataDir) {
      try {
        fs.rmSync(userDataDir, { recursive: true, force: true });
      } catch {
        // Ignore cleanup failures; they are non-fatal.
      }
    }
  }
});
