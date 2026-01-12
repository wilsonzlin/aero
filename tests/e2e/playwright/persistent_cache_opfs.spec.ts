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
      pipelineKey: string | null;
      pipelineOpfsFileExists: boolean;
      pipelineOpfsFileSize: number | null;
      pipelineRoundtripOk: boolean;
      idbShaderRecord: {
        storage: string | null;
        opfsFile: string | null;
        hasWgsl: boolean;
        hasReflection: boolean;
        size: number | null;
      } | null;
      idbPipelineRecord: { storage: string | null; opfsFile: string | null; hasDesc: boolean; size: number | null } | null;
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

        const pipelineKey = typeof result.pipelineKey === "string" ? result.pipelineKey : null;

        const opfsInfo = await page.evaluate(async ({ shaderKey, pipelineKey }: { shaderKey: string; pipelineKey: string | null }) => {
          if (!navigator.storage || typeof navigator.storage.getDirectory !== "function") {
            return { fileSize: null, wgslBytes: null, pipelineFileSize: null };
          }

          try {
            const root = await navigator.storage.getDirectory();
            const dir = await root.getDirectoryHandle("aero-gpu-cache", { create: true });
            const shadersDir = await dir.getDirectoryHandle("shaders");
            const handle = await shadersDir.getFileHandle(`${shaderKey}.json`);
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

            let pipelineFileSize: number | null = null;
            if (pipelineKey) {
              try {
                const pipelinesDir = await dir.getDirectoryHandle("pipelines");
                const pHandle = await pipelinesDir.getFileHandle(`${pipelineKey}.json`);
                const pFile = await pHandle.getFile();
                pipelineFileSize = typeof pFile.size === "number" ? pFile.size : null;
              } catch {
                pipelineFileSize = null;
              }
            }

            return { fileSize: typeof file.size === "number" ? file.size : null, wgslBytes, pipelineFileSize };
          } catch {
            return { fileSize: null, wgslBytes: null, pipelineFileSize: null };
          }
        }, { shaderKey: result.key, pipelineKey });

        const idbRecords = await page.evaluate(
          async ({ shaderKey, pipelineKey }: { shaderKey: string; pipelineKey: string | null }) => {
          const DB_NAME = "aero-gpu-cache";
          const STORE_SHADERS = "shaders";
          const STORE_PIPELINES = "pipelines";

          const db = await new Promise<IDBDatabase>((resolve, reject) => {
            // Open without forcing a schema version so the test stays compatible
            // if the cache bumps `DB_VERSION` in the future.
            const req = indexedDB.open(DB_NAME);
            req.onerror = () => reject(req.error ?? new Error("IndexedDB open failed"));
            req.onsuccess = () => resolve(req.result);
          });

          try {
            const tx = db.transaction([STORE_SHADERS, STORE_PIPELINES], "readonly");
            const shaders = tx.objectStore(STORE_SHADERS);
            const pipelines = tx.objectStore(STORE_PIPELINES);

            const shaderRecord = await new Promise<any>((resolve, reject) => {
              const req = shaders.get(shaderKey);
              req.onerror = () => reject(req.error ?? new Error("IndexedDB get failed"));
              req.onsuccess = () => resolve(req.result ?? null);
            });

            const pipelineRecord = pipelineKey
              ? await new Promise<any>((resolve, reject) => {
                  const req = pipelines.get(pipelineKey);
                  req.onerror = () => reject(req.error ?? new Error("IndexedDB get failed"));
                  req.onsuccess = () => resolve(req.result ?? null);
                })
              : null;

            await new Promise<void>((resolve) => {
              tx.oncomplete = () => resolve();
              tx.onabort = () => resolve(); // best-effort; return what we have
              tx.onerror = () => resolve();
            });

            return {
              shader: shaderRecord
                ? {
                    storage: typeof shaderRecord.storage === "string" ? shaderRecord.storage : null,
                    opfsFile: typeof shaderRecord.opfsFile === "string" ? shaderRecord.opfsFile : null,
                    hasWgsl: typeof shaderRecord.wgsl === "string",
                    hasReflection: shaderRecord.reflection !== undefined,
                    size: typeof shaderRecord.size === "number" && Number.isFinite(shaderRecord.size) ? shaderRecord.size : null,
                  }
                : null,
              pipeline: pipelineRecord
                ? {
                    storage: typeof pipelineRecord.storage === "string" ? pipelineRecord.storage : null,
                    opfsFile: typeof pipelineRecord.opfsFile === "string" ? pipelineRecord.opfsFile : null,
                    hasDesc: pipelineRecord.desc !== undefined,
                    size: typeof pipelineRecord.size === "number" && Number.isFinite(pipelineRecord.size) ? pipelineRecord.size : null,
                  }
                : null,
            };
          } finally {
            db.close();
          }
        },
          { shaderKey: result.key, pipelineKey },
        );

        return {
          key: String(result.key),
          cacheHit: !!result.cacheHit,
          opfsAvailable: !!result.opfsAvailable,
          opfsFileExists: !!result.opfsFileExists,
          opfsFileSize: opfsInfo.fileSize ?? null,
          opfsWgslBytes: opfsInfo.wgslBytes ?? null,
          pipelineKey,
          pipelineOpfsFileExists: !!result.pipelineOpfsFileExists,
          pipelineOpfsFileSize: opfsInfo.pipelineFileSize ?? null,
          pipelineRoundtripOk: !!result.pipelineRoundtripOk,
          idbShaderRecord: idbRecords.shader ?? null,
          idbPipelineRecord: idbRecords.pipeline ?? null,
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
    expect(first.pipelineKey).not.toBeNull();
    expect(first.pipelineOpfsFileExists).toBe(true);
    expect(first.pipelineOpfsFileSize).not.toBeNull();
    expect(first.pipelineOpfsFileSize ?? 0).toBeGreaterThan(256 * 1024);
    expect(first.pipelineRoundtripOk).toBe(true);
    expect(first.idbPipelineRecord?.storage).toBe("opfs");
    expect(first.idbPipelineRecord?.opfsFile).toBe(`${first.pipelineKey}.json`);
    expect(first.idbPipelineRecord?.hasDesc).toBe(false);
    expect(first.idbPipelineRecord?.size).not.toBeNull();
    expect(first.idbPipelineRecord?.size ?? 0).toBeGreaterThan(256 * 1024);

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
    expect(second.pipelineKey).toBe(first.pipelineKey);
    expect(second.pipelineOpfsFileExists).toBe(true);
    expect(second.pipelineOpfsFileSize).not.toBeNull();
    expect(second.pipelineOpfsFileSize ?? 0).toBeGreaterThan(256 * 1024);
    expect(second.pipelineRoundtripOk).toBe(true);
    expect(second.idbPipelineRecord?.storage).toBe("opfs");
    expect(second.idbPipelineRecord?.opfsFile).toBe(`${first.pipelineKey}.json`);
    expect(second.idbPipelineRecord?.hasDesc).toBe(false);
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
