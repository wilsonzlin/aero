import {
  PersistentGpuCache,
  ShaderTranslationCache,
  computeShaderCacheKey,
  computeWebGpuCapsHash,
  compileWgslModule,
} from "./gpu-cache/persistent_cache.ts";

function logLine(line) {
  console.log(line);
  const el = document.getElementById("log");
  if (el) el.textContent += `${line}\n`;
}

async function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

async function tryInitWebGpu() {
  if (!navigator.gpu) return null;
  try {
    const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
    if (!adapter) return null;
    const device = await adapter.requestDevice();
    return { adapter, device };
  } catch {
    return null;
  }
}

function buildValidWgsl() {
  // Minimal WGSL that should compile in all WebGPU implementations.
  return `
@vertex
fn vs_main(@builtin(vertex_index) vertex_index : u32) -> @builtin(position) vec4<f32> {
  // 3 hard-coded vertices (full-screen-ish triangle)
  var pos = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 3.0, -1.0),
    vec2<f32>(-1.0,  3.0)
  );
  let xy = pos[vertex_index];
  return vec4<f32>(xy, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
  return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}
`.trimStart();
}

function buildLargeWgsl(minBytes) {
  const base = buildValidWgsl();
  // WGSL accepts `//` comments; repeating these lines is a harmless way to grow
  // the payload without changing semantics.
  const padLine = "// aero shader cache demo padding .................................................................\n";
  const enc = new TextEncoder();
  const baseBytes = enc.encode(base).byteLength;
  const padBytes = enc.encode(padLine).byteLength;
  const repeats = Math.max(0, Math.ceil((minBytes - baseBytes) / padBytes));
  return padLine.repeat(repeats) + base;
}

async function translateDxbcToWgslSlow(_dxbcBytes, opts) {
  // Simulate an expensive DXBC->WGSL translation pass.
  await sleep(300);
  const wgsl = opts?.large ? buildLargeWgsl(310 * 1024) : buildValidWgsl();
  return {
    wgsl,
    reflection: {
      // Real implementation would store bind group layout metadata, etc.
      bindings: [],
    },
  };
}

async function main() {
  const params = new URLSearchParams(location.search);
  const large = params.get("large") === "1";

  // Ensure deterministic output for tests.
  const dxbc = new Uint8Array([0x44, 0x58, 0x42, 0x43, 1, 2, 3, 4, 5, 6, 7, 8]);
  const webgpu = await tryInitWebGpu();
  const device = webgpu?.device ?? null;
  const capsHash = webgpu ? await computeWebGpuCapsHash(webgpu.adapter) : "no-webgpu";
  const flags = { halfPixelCenter: false, capsHash };
  const key = await computeShaderCacheKey(dxbc, flags);

  const cache = await PersistentGpuCache.open({
    shaderLimits: { maxEntries: 64, maxBytes: 4 * 1024 * 1024 },
    pipelineLimits: { maxEntries: 256, maxBytes: 4 * 1024 * 1024 },
  });
  cache.resetTelemetry();
  const shaderCache = new ShaderTranslationCache(cache);

  const t0 = performance.now();
  let cacheHit = false;
  let payload;
  const result = await shaderCache.getOrTranslate(
    dxbc,
    flags,
    async () => {
      logLine("shader_translate: begin");
      const out = await translateDxbcToWgslSlow(dxbc, { large });
      logLine("shader_translate: end");
      return out;
    },
    device
      ? {
          validateWgsl: async (wgsl) => {
            // Cache-hit corruption defense: validate against current implementation.
            const compile = await compileWgslModule(device, wgsl);
            return !!compile.ok;
          },
        }
      : undefined,
  );
  payload = result.value;
  cacheHit = result.source === "persistent";
  logLine(`shader_cache: ${cacheHit ? "hit" : "miss"} key=${key}`);
  const t1 = performance.now();
  if (device) {
    // Validate cached WGSL against current browser implementation.
    const compile = await compileWgslModule(device, payload.wgsl);
    if (!compile.ok) {
      logLine("wgsl_compile: failed; invalidating cache entry and retranslating");
      await cache.deleteShader(key);
      logLine("shader_translate: begin");
      payload = await translateDxbcToWgslSlow(dxbc, { large });
      logLine("shader_translate: end");
      await cache.putShader(key, payload);
      await compileWgslModule(device, payload.wgsl);
    } else {
      logLine("wgsl_compile: ok");
    }
  } else {
    logLine("wgsl_compile: skipped (WebGPU unavailable)");
  }

  const translationMs = t1 - t0;
  logLine(`shader_cache: done hit=${cacheHit} translation_ms=${translationMs.toFixed(1)}`);

  let opfsAvailable = false;
  let opfsFileExists = false;
  if (large) {
    try {
      if (navigator.storage && typeof navigator.storage.getDirectory === "function") {
        const root = await navigator.storage.getDirectory();
        const dir = await root.getDirectoryHandle("aero-gpu-cache", { create: true });
        const shadersDir = await dir.getDirectoryHandle("shaders");
        opfsAvailable = true;
        try {
          await shadersDir.getFileHandle(`${key}.json`);
          opfsFileExists = true;
        } catch {
          opfsFileExists = false;
        }
      }
    } catch {
      opfsAvailable = false;
      opfsFileExists = false;
    }
  }

  // Expose results for Playwright.
  window.__shaderCacheDemo = {
    key,
    cacheHit,
    translationMs,
    telemetry: cache.getTelemetry(),
    opfsAvailable,
    opfsFileExists,
  };
}

main().catch((err) => {
  console.error(err);
  window.__shaderCacheDemo = { error: String(err) };
});
