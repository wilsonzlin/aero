import { AerogpuCmdWriter, AerogpuShaderStage } from "../emulator/protocol/aerogpu/aerogpu_cmd";
import { createGpuWorker } from "./src/main/createGpuWorker";
import { formatOneLineError } from "./src/text";

type ShaderCacheCounters = {
  translateCalls: number;
  persistentHits: number;
  persistentMisses: number;
  cacheDisabled?: boolean;
};

declare global {
  interface Window {
    __d3d9ShaderCacheDemo?: ShaderCacheCounters & {
      backend: string;
      d3d9TranslatorCacheVersion: number;
      capsHash?: string;
      error?: string;
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function withTimeout<T>(p: Promise<T>, ms: number, label: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms);
    p.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (err) => {
        clearTimeout(timer);
        reject(err);
      },
    );
  });
}

function u32WordsToLeBytes(words: readonly number[]): Uint8Array {
  const buf = new ArrayBuffer(words.length * 4);
  const dv = new DataView(buf);
  for (let i = 0; i < words.length; i += 1) {
    dv.setUint32(i * 4, words[i]! >>> 0, true);
  }
  return new Uint8Array(buf);
}

function isFiniteNumber(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v);
}

function tryParseBool(v: unknown): boolean | undefined {
  if (typeof v === "boolean") return v;
  if (isFiniteNumber(v)) return v !== 0;
  return undefined;
}

function tryParseCounters(value: unknown): ShaderCacheCounters | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;

  const cacheDisabled = tryParseBool(record.d3d9_shader_cache_disabled);

  const d3d9 = {
    translateCalls: record.d3d9_shader_translate_calls,
    persistentHits: record.d3d9_shader_cache_persistent_hits,
    persistentMisses: record.d3d9_shader_cache_persistent_misses,
  };
  if (isFiniteNumber(d3d9.translateCalls) && isFiniteNumber(d3d9.persistentHits) && isFiniteNumber(d3d9.persistentMisses)) {
    return {
      translateCalls: d3d9.translateCalls,
      persistentHits: d3d9.persistentHits,
      persistentMisses: d3d9.persistentMisses,
      cacheDisabled,
    };
  }

  const snake = {
    translateCalls: record.translate_calls,
    persistentHits: record.persistent_hits,
    persistentMisses: record.persistent_misses,
  };
  if (isFiniteNumber(snake.translateCalls) && isFiniteNumber(snake.persistentHits) && isFiniteNumber(snake.persistentMisses)) {
    return {
      translateCalls: snake.translateCalls,
      persistentHits: snake.persistentHits,
      persistentMisses: snake.persistentMisses,
      cacheDisabled,
    };
  }

  const camel = {
    translateCalls: record.translateCalls,
    persistentHits: record.persistentHits,
    persistentMisses: record.persistentMisses,
  };
  if (isFiniteNumber(camel.translateCalls) && isFiniteNumber(camel.persistentHits) && isFiniteNumber(camel.persistentMisses)) {
    return {
      translateCalls: camel.translateCalls,
      persistentHits: camel.persistentHits,
      persistentMisses: camel.persistentMisses,
      cacheDisabled,
    };
  }

  return null;
}

function tryParseTranslatorCacheVersion(value: unknown): number | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;

  const candidates = [
    record.d3d9TranslatorCacheVersion,
    record.d3d9_translator_cache_version,
    record.d3d9TranslatorVersion,
    record.d3d9_translator_version,
  ];

  for (const candidate of candidates) {
    if (isFiniteNumber(candidate)) return candidate;
  }
  return null;
}

function findShaderCacheCounters(root: unknown): ShaderCacheCounters | null {
  const seen = new Set<unknown>();

  const walk = (value: unknown): ShaderCacheCounters | null => {
    const parsed = tryParseCounters(value);
    if (parsed) return parsed;

    if (!value || typeof value !== "object") return null;
    if (seen.has(value)) return null;
    seen.add(value);

    if (Array.isArray(value)) {
      for (const item of value) {
        const found = walk(item);
        if (found) return found;
      }
      return null;
    }

    for (const child of Object.values(value as Record<string, unknown>)) {
      const found = walk(child);
      if (found) return found;
    }
    return null;
  };

  return walk(root);
}

function findTranslatorCacheVersion(root: unknown): number | null {
  const seen = new Set<unknown>();

  const walk = (value: unknown): number | null => {
    const parsed = tryParseTranslatorCacheVersion(value);
    if (parsed !== null) return parsed;

    if (!value || typeof value !== "object") return null;
    if (seen.has(value)) return null;
    seen.add(value);

    if (Array.isArray(value)) {
      for (const item of value) {
        const found = walk(item);
        if (found !== null) return found;
      }
      return null;
    }

    for (const child of Object.values(value as Record<string, unknown>)) {
      const found = walk(child);
      if (found !== null) return found;
    }
    return null;
  };

  return walk(root);
}

function subtractCounters(after: ShaderCacheCounters, before: ShaderCacheCounters): ShaderCacheCounters {
  return {
    translateCalls: Math.max(0, after.translateCalls - before.translateCalls),
    persistentHits: Math.max(0, after.persistentHits - before.persistentHits),
    persistentMisses: Math.max(0, after.persistentMisses - before.persistentMisses),
    cacheDisabled: after.cacheDisabled,
  };
}

async function main(): Promise<void> {
  const status = $("status");
  const canvas = $("frame");
  if (!(canvas instanceof HTMLCanvasElement)) {
    throw new Error("Canvas element #frame not found");
  }

  const qs = new URLSearchParams(globalThis.location?.search ?? "");
  const requestedBackend = (qs.get("backend") ?? qs.get("forceBackend") ?? "").trim();
  const forceBackend =
    requestedBackend === "webgpu" || requestedBackend === "webgl2_wgpu" || requestedBackend === "webgl2_raw"
      ? requestedBackend
      : "webgl2_wgpu";

  const cssWidth = 4;
  const cssHeight = 4;
  const devicePixelRatio = 1;
  canvas.width = cssWidth * devicePixelRatio;
  canvas.height = cssHeight * devicePixelRatio;
  canvas.style.width = `${cssWidth}px`;
  canvas.style.height = `${cssHeight}px`;

  let baseline: ShaderCacheCounters = { translateCalls: 0, persistentHits: 0, persistentMisses: 0 };
  let d3d9TranslatorCacheVersion: number | null = null;
  let baselineReadyResolve!: () => void;
  const baselineReady = new Promise<void>((resolve) => {
    baselineReadyResolve = resolve;
  });

  let capsHash = "";
  let submitted = false;
  let resolveCounters!: (c: ShaderCacheCounters) => void;
  let rejectCounters!: (err: unknown) => void;
  const countersPromise = new Promise<ShaderCacheCounters>((resolve, reject) => {
    resolveCounters = resolve;
    rejectCounters = reject;
  });
  // Ensure early worker failures don't surface as unhandled rejections before we `await` below.
  void countersPromise.catch(() => {});

  const gpu = createGpuWorker({
    canvas,
    width: cssWidth,
    height: cssHeight,
    devicePixelRatio,
    gpuOptions: {
      // Force a backend so:
      // - the aero-gpu wasm module is loaded (real D3D9 executor path)
      // - wasm stats are forwarded via GpuRuntimeStatsMessage.wasm
      //
      // Default: webgl2_wgpu (most deterministic + widely available).
      // Override via `?backend=webgpu|webgl2_wgpu|webgl2_raw` for debugging.
      forceBackend,
    },
    onError: (msg) => {
      rejectCounters(new Error(msg.message));
    },
    onStats: (msg) => {
      if (d3d9TranslatorCacheVersion === null) {
        const foundVersion = findTranslatorCacheVersion(msg.wasm);
        if (foundVersion !== null) d3d9TranslatorCacheVersion = foundVersion;
      }
      const wasm = msg.wasm as unknown;
      if (!capsHash && wasm && typeof wasm === "object" && !Array.isArray(wasm)) {
        const record = wasm as Record<string, unknown>;
        const vSnake = record.d3d9_shader_cache_caps_hash;
        const vCamel = record.d3d9ShaderCacheCapsHash;
        if (typeof vSnake === "string") capsHash = vSnake;
        else if (typeof vCamel === "string") capsHash = vCamel;
      }
      const found = findShaderCacheCounters(msg.wasm);
      if (!found) return;

      if (!submitted) {
        baseline = found;
        baselineReadyResolve();
        return;
      }

      const delta = subtractCounters(found, baseline);
      if (delta.translateCalls > 0 || delta.persistentHits > 0 || delta.persistentMisses > 0) {
        resolveCounters(delta);
      }
    },
  });

  const ready = await gpu.ready;

  // Allow the worker to emit at least one telemetry tick before submission so we can
  // record baseline counters (best-effort).
  await Promise.race([baselineReady, sleep(500)]);

  // Minimal D3D9 SM2 token stream:
  //   vs_2_0
  //   mov oPos, v0
  //   end
  //
  // These DWORDs match the helpers in `crates/aero-gpu/tests/aerogpu_d3d9_shader.rs`.
  const d3d9Vs20Words: number[] = [
    0xfffe0200, // vs_2_0
    0x03000001, // mov (len=3 tokens: opcode + 2 params)
    0x400f0000, // dst oPos.xyzw
    0x10e40000, // src v0.xyzw
    0x0000ffff, // end
  ];
  const shaderBytes = u32WordsToLeBytes(d3d9Vs20Words);

  const writer = new AerogpuCmdWriter();
  writer.createShaderDxbc(/* shaderHandle */ 1, AerogpuShaderStage.Vertex, shaderBytes);
  const cmdStream = writer.finish();

  submitted = true;
  await gpu.submitAerogpu(cmdStream.buffer, /* fence */ 1n, undefined, /* contextId */ 0);

  const delta = await withTimeout(countersPromise, 10_000, "Waiting for wasm shader-cache stats");
  const version = d3d9TranslatorCacheVersion ?? -1;
  window.__d3d9ShaderCacheDemo = {
    backend: ready.backendKind,
    d3d9TranslatorCacheVersion: version,
    capsHash,
    ...delta,
  };

  if (status) {
    status.textContent =
      `backend=${ready.backendKind}\n` +
      `d3d9TranslatorCacheVersion=${version}\n` +
      (capsHash ? `capsHash=${capsHash}\n` : "") +
      `translateCalls=${delta.translateCalls}\n` +
      `persistentHits=${delta.persistentHits}\n` +
      `persistentMisses=${delta.persistentMisses}\n` +
      (delta.cacheDisabled === undefined ? "" : `cacheDisabled=${delta.cacheDisabled}\n`);
  }
}

void main().catch((err) => {
  const message = formatOneLineError(err, 512);
  window.__d3d9ShaderCacheDemo = {
    translateCalls: 0,
    persistentHits: 0,
    persistentMisses: 0,
    backend: "unknown",
    d3d9TranslatorCacheVersion: -1,
    capsHash: "",
    error: message,
  };
  const status = $("status");
  if (status) status.textContent = `error: ${message}\n`;
});
