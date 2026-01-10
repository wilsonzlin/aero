import {
  PersistentGpuCache,
  computeShaderCacheKey,
  compileWgslModule,
} from "./gpu/persistent_cache.ts";

function logLine(line) {
  console.log(line);
  const el = document.getElementById("log");
  if (el) el.textContent += `${line}\n`;
}

async function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

async function tryInitWebGpuDevice() {
  if (!navigator.gpu) return null;
  try {
    const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
    if (!adapter) return null;
    return await adapter.requestDevice();
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

async function translateDxbcToWgslSlow(_dxbcBytes) {
  // Simulate an expensive DXBC->WGSL translation pass.
  await sleep(300);
  return {
    wgsl: buildValidWgsl(),
    reflection: {
      // Real implementation would store bind group layout metadata, etc.
      bindings: [],
    },
  };
}

async function main() {
  // Ensure deterministic output for tests.
  const dxbc = new Uint8Array([0x44, 0x58, 0x42, 0x43, 1, 2, 3, 4, 5, 6, 7, 8]);
  const flags = { halfPixelCenter: false, capsHash: "demo-caps-v1" };
  const key = await computeShaderCacheKey(dxbc, flags);

  const cache = await PersistentGpuCache.open({
    shaderLimits: { maxEntries: 64, maxBytes: 4 * 1024 * 1024 },
    pipelineLimits: { maxEntries: 256, maxBytes: 4 * 1024 * 1024 },
  });

  const t0 = performance.now();
  const cached = await cache.getShader(key);

  let cacheHit = false;
  let payload;
  if (cached) {
    cacheHit = true;
    payload = cached;
    logLine(`shader_cache: hit key=${key}`);
  } else {
    logLine(`shader_cache: miss key=${key}`);
    logLine("shader_translate: begin");
    payload = await translateDxbcToWgslSlow(dxbc);
    logLine("shader_translate: end");
    await cache.putShader(key, payload);
  }
  const t1 = performance.now();

  const device = await tryInitWebGpuDevice();
  if (device) {
    // Validate cached WGSL against current browser implementation.
    const compile = await compileWgslModule(device, payload.wgsl);
    if (!compile.ok) {
      logLine("wgsl_compile: failed; invalidating cache entry and retranslating");
      await cache.deleteShader(key);
      logLine("shader_translate: begin");
      payload = await translateDxbcToWgslSlow(dxbc);
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

  // Expose results for Playwright.
  window.__shaderCacheDemo = {
    key,
    cacheHit,
    translationMs,
  };
}

main().catch((err) => {
  console.error(err);
  window.__shaderCacheDemo = { error: String(err) };
});

