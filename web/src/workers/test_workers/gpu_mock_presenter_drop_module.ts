import { SharedFramebufferHeaderIndex } from "../../ipc/shared-layout";

type SharedFramebufferViews = {
  header: Int32Array;
  width: number;
  height: number;
  strideBytes: number;
  pixels: Uint8Array;
};

const getActiveSharedFramebuffer = (): SharedFramebufferViews | null => {
  const shared = (globalThis as unknown as { __aeroSharedFramebuffer?: any }).__aeroSharedFramebuffer as
    | {
        header?: unknown;
        layout?: { width?: unknown; height?: unknown; strideBytes?: unknown };
        slot0?: unknown;
        slot1?: unknown;
      }
    | undefined;
  if (!shared) return null;
  if (!(shared.header instanceof Int32Array)) return null;
  const layout = shared.layout as { width?: unknown; height?: unknown; strideBytes?: unknown } | undefined;
  const width = typeof layout?.width === "number" ? layout.width : 0;
  const height = typeof layout?.height === "number" ? layout.height : 0;
  const strideBytes = typeof layout?.strideBytes === "number" ? layout.strideBytes : 0;
  if (width <= 0 || height <= 0 || strideBytes <= 0) return null;

  const active = Atomics.load(shared.header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
  const slot = active === 0 ? shared.slot0 : shared.slot1;
  if (!(slot instanceof Uint8Array)) return null;
  return { header: shared.header, width, height, strideBytes, pixels: slot };
};

// Signal that the module has been imported (so tests know `presentFn` is installed).
postMessage({ type: "mock_presenter_loaded" });

// A presenter module that intentionally *drops* every frame (returns false).
export function present(): boolean {
  const fb = getActiveSharedFramebuffer();
  if (!fb) {
    postMessage({ type: "mock_present", ok: false, reason: "no_shared_framebuffer" });
    return false;
  }

  const px = fb.pixels;
  const firstPixel =
    px.byteLength >= 4
      ? (((px[0] ?? 0) | ((px[1] ?? 0) << 8) | ((px[2] ?? 0) << 16) | ((px[3] ?? 0) << 24)) >>> 0)
      : 0;
  const seq = Atomics.load(fb.header, SharedFramebufferHeaderIndex.FRAME_SEQ) >>> 0;

  postMessage({
    type: "mock_present",
    ok: false,
    firstPixel,
    seq,
    width: fb.width,
    height: fb.height,
    strideBytes: fb.strideBytes,
  });
  return false;
}

