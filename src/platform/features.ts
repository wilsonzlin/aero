export type PlatformFeatureReport = {
  /**
   * Whether the current JS execution context is cross-origin isolated.
   *
   * This is required for `SharedArrayBuffer` to be exposed in most browsers.
   * See: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/SharedArrayBuffer#security_requirements
   */
  crossOriginIsolated: boolean;
  /** Whether `SharedArrayBuffer` is available in the current context. */
  sharedArrayBuffer: boolean;
  /**
   * Best-effort detection for WebAssembly SIMD support.
   *
   * Uses `WebAssembly.validate` against a tiny module containing a SIMD opcode.
   */
  wasmSimd: boolean;
  /**
   * Best-effort detection for WebAssembly threads support.
   *
   * For Aero, we treat "threads available" as requiring:
   * - `SharedArrayBuffer`
   * - `Atomics`
   * - cross-origin isolation
   */
  wasmThreads: boolean;
  /** Whether WebGPU is exposed (`navigator.gpu`). */
  webgpu: boolean;
  /**
   * Whether WebUSB is exposed (`navigator.usb`) in this context.
   *
   * WebUSB is Chromium-only and only available in secure contexts
   * (HTTPS / localhost). In practice, browsers hide `navigator.usb` entirely
   * when it is unavailable, so a simple presence check is sufficient.
   */
  webusb: boolean;
  /** Whether OPFS is exposed (`navigator.storage.getDirectory`). */
  opfs: boolean;
  /**
   * Whether OPFS sync access handles *appear* to be available
   * (`FileSystemFileHandle.prototype.createSyncAccessHandle`).
   *
   * Note: `createSyncAccessHandle()` is worker-only; even if this is true, the
   * caller still needs to run in a dedicated worker to use it.
   */
  opfsSyncAccessHandle: boolean;
  /** Whether AudioWorklet is available. */
  audioWorklet: boolean;
  /** Whether `OffscreenCanvas` is available. */
  offscreenCanvas: boolean;
  /**
   * Whether dynamic WebAssembly compilation is allowed in this context.
   *
   * In practice, this is primarily gated by Content Security Policy (CSP).
   * If `script-src` does not include `'wasm-unsafe-eval'`, browsers may refuse
   * to compile or instantiate any WebAssembly modules (including JIT blocks).
   *
   * Aero's Tier-1/2 JIT must be disabled when this is false.
   */
  jit_dynamic_wasm: boolean;
};

const WASM_SIMD_VALIDATION_BYTES = new Uint8Array([
  // From docs/11-browser-apis.md (WASM SIMD feature detection snippet).
  // (module (func (result v128) i32.const 0 i32x4.splat))
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01,
  0x7b, 0x03, 0x02, 0x01, 0x00, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x41, 0x00, 0xfd, 0x11,
  0x0b,
]);

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

function detectWasmSimd(): boolean {
  if (typeof WebAssembly === 'undefined' || typeof WebAssembly.validate !== 'function') {
    return false;
  }
  try {
    return WebAssembly.validate(WASM_SIMD_VALIDATION_BYTES);
  } catch {
    return false;
  }
}

function detectWasmThreads(crossOriginIsolated: boolean, sharedArrayBuffer: boolean): boolean {
  if (!crossOriginIsolated || !sharedArrayBuffer) return false;
  if (typeof Atomics === 'undefined') return false;
  if (typeof WebAssembly === 'undefined' || typeof WebAssembly.Memory !== 'function') return false;

  try {
    const mem = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    return mem.buffer instanceof SharedArrayBuffer;
  } catch {
    return false;
  }
}

function detectDynamicWasmCompilation(): boolean {
  if (typeof WebAssembly === 'undefined' || typeof WebAssembly.Module !== 'function') {
    return false;
  }
  try {
    // `new WebAssembly.Module(...)` is synchronous and tends to fail fast with a CSP error
    // when `'wasm-unsafe-eval'` is not present in `script-src`.
    new WebAssembly.Module(WASM_EMPTY_MODULE_BYTES);
    return true;
  } catch {
    return false;
  }
}

function getAudioContextCtor(): typeof AudioContext | undefined {
  // Safari uses webkitAudioContext.
  return (
    (globalThis as typeof globalThis & { webkitAudioContext?: typeof AudioContext }).AudioContext ??
    (globalThis as typeof globalThis & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
  );
}

export function detectPlatformFeatures(): PlatformFeatureReport {
  const crossOriginIsolated = (globalThis as typeof globalThis & { crossOriginIsolated?: boolean })
    .crossOriginIsolated === true;
  const sharedArrayBuffer = typeof SharedArrayBuffer !== 'undefined';
  const wasmSimd = detectWasmSimd();
  const wasmThreads = detectWasmThreads(crossOriginIsolated, sharedArrayBuffer);
  const jit_dynamic_wasm = detectDynamicWasmCompilation();

  const webgpu = typeof navigator !== 'undefined' && !!(navigator as Navigator & { gpu?: unknown }).gpu;
  const webusb = typeof navigator !== 'undefined' && typeof navigator.usb !== 'undefined';
  const opfs =
    typeof navigator !== 'undefined' &&
    typeof navigator.storage !== 'undefined' &&
    typeof (navigator.storage as StorageManager & { getDirectory?: unknown }).getDirectory === 'function';
  const opfsSyncAccessHandle =
    opfs &&
    typeof (globalThis as typeof globalThis & { FileSystemFileHandle?: unknown }).FileSystemFileHandle !== 'undefined' &&
    typeof (
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).FileSystemFileHandle?.prototype?.createSyncAccessHandle
    ) === 'function';

  const audioContextCtor = getAudioContextCtor();
  const audioWorklet = typeof AudioWorkletNode !== 'undefined' && typeof audioContextCtor !== 'undefined';

  const offscreenCanvas = typeof OffscreenCanvas !== 'undefined';

  return {
    crossOriginIsolated,
    sharedArrayBuffer,
    wasmSimd,
    wasmThreads,
    jit_dynamic_wasm,
    webgpu,
    webusb,
    opfs,
    opfsSyncAccessHandle,
    audioWorklet,
    offscreenCanvas,
  };
}

export const platformFeatures: PlatformFeatureReport = detectPlatformFeatures();

/**
 * Returns actionable, human-friendly messages describing why Aero can't run
 * fully on this browser/context.
 *
 * Note: the UI should still load when these are missing; this is informational.
 */
export function explainMissingRequirements(report: PlatformFeatureReport = platformFeatures): string[] {
  const messages: string[] = [];

  if (!report.crossOriginIsolated) {
    messages.push(
      'This page is not cross-origin isolated. Aero needs COOP/COEP headers (Cross-Origin-Opener-Policy: same-origin and Cross-Origin-Embedder-Policy: require-corp) to enable SharedArrayBuffer and WASM threads.',
    );
  }

  if (!report.sharedArrayBuffer) {
    messages.push(
      'SharedArrayBuffer is unavailable. Aero uses shared memory between workers; ensure you\'re in a modern browser and the page is served with COOP/COEP.',
    );
  }

  if (!report.wasmSimd) {
    messages.push(
      'WebAssembly SIMD is unavailable. Aero requires SIMD for acceptable performance; update to a modern browser version with WASM SIMD enabled.',
    );
  }

  if (!report.wasmThreads) {
    messages.push(
      "WebAssembly threads are unavailable. Aero's design relies on multithreading; this typically requires cross-origin isolation, SharedArrayBuffer, and Atomics.",
    );
  }

  if (!report.jit_dynamic_wasm) {
    messages.push(
      "Dynamic WebAssembly compilation is blocked (likely by CSP). Aero's JIT tiers require `script-src 'wasm-unsafe-eval'`; otherwise the host must fall back to an interpreter-only mode.",
    );
  }

  if (!report.webgpu) {
    messages.push(
      'WebGPU is unavailable. Aero requires WebGPU for GPU-accelerated rendering (Chrome/Edge 113+; Firefox/Safari support may require flags).',
    );
  }

  if (!report.opfs) {
    messages.push(
      'Origin Private File System (OPFS) is unavailable. Aero uses OPFS for fast, persistent disk image I/O; update your browser or enable the File System Access APIs.',
    );
  }

  if (!report.opfsSyncAccessHandle) {
    messages.push(
      "OPFS SyncAccessHandle is unavailable. Aero's boot-critical Rust disk/controller stack (aero-storage::VirtualDisk + AHCI/IDE) requires synchronous disk I/O via FileSystemFileHandle.createSyncAccessHandle() in a dedicated worker; IndexedDB is async-only and cannot be used as a drop-in substitute. Use a browser with OPFS SyncAccessHandle support (typically Chromium) or run the demo mode without a disk image.",
    );
  }

  if (!report.audioWorklet) {
    messages.push(
      "AudioWorklet is unavailable. Aero's audio output requires AudioWorklet; update your browser or disable audio output for now.",
    );
  }

  if (!report.offscreenCanvas) {
    messages.push(
      'OffscreenCanvas is unavailable. Aero uses OffscreenCanvas for worker-driven rendering; update your browser or use a configuration that renders on the main thread.',
    );
  }

  return messages;
}
