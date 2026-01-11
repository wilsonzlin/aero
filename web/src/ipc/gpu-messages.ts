/**
 * Minimal message/types used by the legacy `aero-gpu` wasm presenter wrapper (`web/src/wasm/aero-gpu.ts`).
 *
 * These types are intentionally small and self-contained so the wasm presenter can be
 * imported by smoke tests or experimental workers without pulling in the full runtime
 * GPU protocol.
 */

export type BackendKind = "webgpu" | "webgl2";

export interface GpuWorkerInitOptions {
  /**
   * When true, attempt WebGPU initialization (unless disabled). When false, the
   * presenter may prefer WebGL2 first.
   */
  preferWebGpu?: boolean;
  /**
   * When true, treat WebGPU as unavailable.
   */
  disableWebGpu?: boolean;
  /**
   * WebGPU feature strings to require when using the WebGPU backend.
   *
   * Unknown features should be rejected by the implementation.
   */
  requiredFeatures?: string[];

  /**
   * Optional output canvas size in CSS pixels. When unset, defaults to the source
   * framebuffer width/height.
   */
  outputWidth?: number;
  outputHeight?: number;

  /**
   * How the framebuffer should be mapped into the output canvas when sizes differ.
   */
  scaleMode?: "stretch" | "fit" | "integer";

  /**
   * Texture filtering mode.
   */
  filter?: "nearest" | "linear";

  /**
   * Clear color used for letterboxing/pillarboxing (RGBA floats).
   */
  clearColor?: [number, number, number, number];
}

export interface GpuAdapterInfo {
  vendor?: string;
  renderer?: string;
  description?: string;
}

export interface FrameTimingsReport {
  frame_index: number;
  backend: BackendKind;
  cpu_encode_us: number;
  cpu_submit_us: number;
  gpu_us?: number;
}
