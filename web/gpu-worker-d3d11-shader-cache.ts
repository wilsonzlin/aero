import initD3d11, { run_d3d11_shader_cache_demo } from "./src/wasm/aero-d3d11";
import { computeWebGpuCapsHash } from "./gpu-cache/persistent_cache.ts";

type ShaderCacheCounters = {
  translateCalls: number;
  persistentHits: number;
  persistentMisses: number;
  cacheDisabled: boolean;
};

declare global {
  interface Window {
    __d3d11ShaderCacheDemo?: ShaderCacheCounters & {
      backend: string;
      d3d11TranslatorCacheVersion: number;
      capsHash?: string;
      source?: string;
      error?: string;
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

async function tryComputeCapsHash(): Promise<string> {
  try {
    const gpu: any = (globalThis as any).navigator?.gpu;
    if (!gpu?.requestAdapter) return "";
    const adapter = await gpu.requestAdapter();
    if (!adapter) return "";
    return await computeWebGpuCapsHash(adapter);
  } catch {
    return "";
  }
}

function num(v: unknown): number {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? n : 0;
}

async function main(): Promise<void> {
  const status = $("status");

  const capsHash = await tryComputeCapsHash();

  await initD3d11();
  const result = (await run_d3d11_shader_cache_demo(capsHash || null)) as any;

  const translateCalls = num(result?.translateCalls);
  const persistentHits = num(result?.persistentHits);
  const persistentMisses = num(result?.persistentMisses);
  const cacheDisabled = Boolean(result?.cacheDisabled);
  const d3d11TranslatorCacheVersion = num(result?.d3d11TranslatorCacheVersion);
  const source = typeof result?.source === "string" ? result.source : "";

  window.__d3d11ShaderCacheDemo = {
    backend: "d3d11",
    translateCalls,
    persistentHits,
    persistentMisses,
    cacheDisabled,
    d3d11TranslatorCacheVersion,
    capsHash,
    source,
  };

  if (status) {
    status.textContent =
      `backend=d3d11\n` +
      `d3d11TranslatorCacheVersion=${d3d11TranslatorCacheVersion}\n` +
      (capsHash ? `capsHash=${capsHash}\n` : "") +
      (source ? `source=${source}\n` : "") +
      `translateCalls=${translateCalls}\n` +
      `persistentHits=${persistentHits}\n` +
      `persistentMisses=${persistentMisses}\n` +
      `cacheDisabled=${cacheDisabled}\n`;
  }
}

void main().catch((err) => {
  const message = err instanceof Error ? err.message : String(err);
  window.__d3d11ShaderCacheDemo = {
    translateCalls: 0,
    persistentHits: 0,
    persistentMisses: 0,
    cacheDisabled: true,
    backend: "d3d11",
    d3d11TranslatorCacheVersion: -1,
    capsHash: "",
    error: message,
  };
  const status = $("status");
  if (status) status.textContent = `error: ${message}\n`;
});

