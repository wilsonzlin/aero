import {
  PersistentGpuCache,
  ShaderTranslationCache,
  computePipelineCacheKey,
  computeShaderCacheKey,
  computeWebGpuCapsHash,
  compileWgslModule,
} from "./gpu-cache/persistent_cache.ts";
import { formatOneLineError } from "./src/text";

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

function buildLargePipelineDescriptor(minBytes) {
  const pad = "x".repeat(minBytes);
  return { kind: "demo-pipeline-desc", version: 1, pad };
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
  // Large mode is meant to exercise the persistent cache spill-to-OPFS path; it
  // doesn't need WebGPU and can be more stable if we avoid compiling an enormous
  // shader module.
  const webgpu = large ? null : await tryInitWebGpu();
  const device = webgpu?.device ?? null;
  const capsHash = webgpu ? await computeWebGpuCapsHash(webgpu.adapter) : "no-webgpu";
  // Only include `large` in the cache key when enabled so default behavior/key
  // shape remains unchanged.
  const flags = large ? { halfPixelCenter: false, capsHash, large: true } : { halfPixelCenter: false, capsHash };
  const key = await computeShaderCacheKey(dxbc, flags);

  const cache = await PersistentGpuCache.open({
    shaderLimits: { maxEntries: 64, maxBytes: 4 * 1024 * 1024 },
    pipelineLimits: { maxEntries: 256, maxBytes: 4 * 1024 * 1024 },
  });
  try {
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
      device && !large
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
    if (device && !large) {
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
      logLine(device ? "wgsl_compile: skipped (large mode)" : "wgsl_compile: skipped (WebGPU unavailable)");
    }

    const translationMs = t1 - t0;
    logLine(`shader_cache: done hit=${cacheHit} translation_ms=${translationMs.toFixed(1)}`);

    let opfsAvailable = false;
    let opfsFileExists = false;
    let pipelineKey = null;
    let pipelineOpfsFileExists = false;
    let pipelineRoundtripOk = false;
    let pipelineCacheHit = false;
    if (large) {
      if (navigator.storage && typeof navigator.storage.getDirectory === "function") {
        try {
          const root = await navigator.storage.getDirectory();
          const dir = await root.getDirectoryHandle("aero-gpu-cache", { create: true });
          opfsAvailable = true;

          try {
            const shadersDir = await dir.getDirectoryHandle("shaders");
            await shadersDir.getFileHandle(`${key}.json`);
            opfsFileExists = true;
          } catch {
            opfsFileExists = false;
          }

          // Also write a large pipeline descriptor to exercise the pipeline spillover path.
          try {
            const pipelineDesc = buildLargePipelineDescriptor(310 * 1024);
            pipelineKey = await computePipelineCacheKey(pipelineDesc);
            // `PersistentGpuCache.open()` warms pipeline descriptors into memory.
            // If the descriptor was persisted previously, this should already be a hit.
            pipelineCacheHit = cache.pipelineDescriptors.has(pipelineKey);
            if (!pipelineCacheHit) {
              await cache.putPipelineDescriptor(pipelineKey, pipelineDesc);
            }
            // Force a persistent read path within the same session.
            cache.pipelineDescriptors.clear();
            const gotPipeline = await cache.getPipelineDescriptor(pipelineKey);
            pipelineRoundtripOk =
              !!gotPipeline &&
              gotPipeline.version === pipelineDesc.version &&
              typeof gotPipeline.pad === "string" &&
              gotPipeline.pad.length === pipelineDesc.pad.length;

            try {
              const pipelinesDir = await dir.getDirectoryHandle("pipelines");
              await pipelinesDir.getFileHandle(`${pipelineKey}.json`);
              pipelineOpfsFileExists = true;
            } catch {
              pipelineOpfsFileExists = false;
            }
          } catch {
            // Ignore; shader OPFS coverage should remain functional even if the
            // pipeline spillover demo path fails.
            pipelineOpfsFileExists = false;
            pipelineRoundtripOk = false;
          }
        } catch {
          opfsAvailable = false;
          opfsFileExists = false;
          pipelineOpfsFileExists = false;
        }
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
      pipelineKey,
      pipelineOpfsFileExists,
      pipelineRoundtripOk,
      pipelineCacheHit,
    };
  } finally {
    // Best-effort: close the IndexedDB handle so persistent-context tests can
    // cleanly shut down without blocked transactions.
    await cache.close();
  }
}

main().catch((err) => {
  console.error(err);
  window.__shaderCacheDemo = { error: formatOneLineError(err, 512) };
});
