import initD3d11, { run_d3d11_shader_cache_demo } from "./src/wasm/aero-d3d11";
import { computeWebGpuCapsHash } from "./gpu-cache/persistent_cache.ts";
import { formatOneLineError } from "./src/text";

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
    const gpu = (globalThis as unknown as { navigator?: { gpu?: unknown } }).navigator?.gpu;
    if (!gpu || typeof gpu !== "object") return "";
    const requestAdapter = (gpu as { requestAdapter?: unknown }).requestAdapter;
    if (typeof requestAdapter !== "function") return "";
    const adapter = await (requestAdapter as (options?: unknown) => Promise<unknown>).call(gpu);
    if (!adapter) return "";
    return await computeWebGpuCapsHash(adapter as GPUAdapter);
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
  const result = (await run_d3d11_shader_cache_demo(capsHash || null)) as unknown;
  const resultRecord = result && typeof result === "object" ? (result as Record<string, unknown>) : {};

  const translateCalls = num(resultRecord["translateCalls"]);
  const persistentHits = num(resultRecord["persistentHits"]);
  const persistentMisses = num(resultRecord["persistentMisses"]);
  const cacheDisabled = Boolean(resultRecord["cacheDisabled"]);
  const d3d11TranslatorCacheVersion = num(resultRecord["d3d11TranslatorCacheVersion"]);
  const source = typeof resultRecord["source"] === "string" ? resultRecord["source"] : "";

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
  const message = formatOneLineError(err, 512);
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
