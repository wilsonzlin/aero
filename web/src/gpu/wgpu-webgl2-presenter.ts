import type { Presenter, PresenterInitOptions, PresenterScreenshot } from './presenter';
import { PresenterError } from './presenter';

/**
 * Placeholder for the "wgpu WebGL2 backend" presenter. The real implementation
 * lives in the Rust/wasm-bindgen + wgpu stack and is intentionally not part of
 * the raw WebGL2 contingency layer.
 */
export class WgpuWebGl2Presenter implements Presenter {
  public readonly backend = 'webgl2_wgpu' as const;

  public init(_canvas: OffscreenCanvas, _width: number, _height: number, _dpr: number, _opts?: PresenterInitOptions): void {
    throw new PresenterError(
      'wgpu_backend_unavailable',
      'wgpu WebGL2 backend presenter is not bundled. Use backend=webgl2_raw for a guaranteed fallback.',
    );
  }

  public resize(_width: number, _height: number, _dpr: number): void {
    throw new PresenterError(
      'wgpu_backend_unavailable',
      'wgpu WebGL2 backend presenter is not bundled. Use backend=webgl2_raw for a guaranteed fallback.',
    );
  }

  public present(_frame: number | ArrayBuffer | ArrayBufferView, _stride: number): void {
    throw new PresenterError(
      'wgpu_backend_unavailable',
      'wgpu WebGL2 backend presenter is not bundled. Use backend=webgl2_raw for a guaranteed fallback.',
    );
  }

  public screenshot(): PresenterScreenshot {
    throw new PresenterError(
      'wgpu_backend_unavailable',
      'wgpu WebGL2 backend presenter is not bundled. Use backend=webgl2_raw for a guaranteed fallback.',
    );
  }
}

