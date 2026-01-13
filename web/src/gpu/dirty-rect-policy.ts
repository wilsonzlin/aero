import type { DirtyRect, SharedFramebufferLayout } from "../ipc/shared-layout";

type DirtyRectUploadPolicyOptions = {
  /**
   * Max number of rects we will attempt to upload individually before forcing a
   * full-frame upload.
   */
  maxRects?: number;
  /**
   * If dirty-rect uploads would transfer >= `fullFrameBytes * threshold`,
   * force a full-frame upload instead.
   */
  fullFrameRatioThreshold?: number;
};

const BYTES_PER_PIXEL_RGBA8 = 4;
const DEFAULT_MAX_RECTS = 1024;
const DEFAULT_FULL_FRAME_RATIO_THRESHOLD = 0.75;

function alignUp(value: number, align: number): number {
  if (align <= 0) return value;
  return Math.ceil(value / align) * align;
}

function bytesPerRowForUpload(rowBytes: number, copyHeight: number, bytesPerRowAlignment: number): number {
  // WebGPU's bytesPerRow alignment requirement only applies when multiple rows
  // are present (bytesPerRow can be omitted for single-row uploads).
  if (copyHeight <= 1) return rowBytes;
  return alignUp(rowBytes, bytesPerRowAlignment);
}

function requiredDataLen(bytesPerRow: number, rowBytes: number, copyHeight: number): number {
  if (copyHeight <= 0) return 0;
  // The last row does not require padding out to bytesPerRow.
  return bytesPerRow * (copyHeight - 1) + rowBytes;
}

function clampInt(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.trunc(value)));
}

export function estimateFullFrameUploadBytes(width: number, height: number, bytesPerRowAlignment: number): number {
  const rowBytes = width * BYTES_PER_PIXEL_RGBA8;
  const bytesPerRow = bytesPerRowForUpload(rowBytes, height, bytesPerRowAlignment);
  return requiredDataLen(bytesPerRow, rowBytes, height);
}

export function estimateTextureUploadBytes(
  layout: SharedFramebufferLayout | null,
  dirtyRects: DirtyRect[] | null,
  bytesPerRowAlignment: number,
): number {
  if (!layout) return 0;

  const fullRect: DirtyRect = { x: 0, y: 0, w: layout.width, h: layout.height };
  const rects = dirtyRects == null ? [fullRect] : dirtyRects.length === 0 ? ([] as DirtyRect[]) : dirtyRects;

  let total = 0;
  for (const rect of rects) {
    const x = clampInt(rect.x, 0, layout.width);
    const y = clampInt(rect.y, 0, layout.height);
    const w = clampInt(rect.w, 0, layout.width - x);
    const h = clampInt(rect.h, 0, layout.height - y);
    if (w === 0 || h === 0) continue;

    const rowBytes = w * BYTES_PER_PIXEL_RGBA8;
    const bytesPerRow = bytesPerRowForUpload(rowBytes, h, bytesPerRowAlignment);
    total += requiredDataLen(bytesPerRow, rowBytes, h);
  }

  return total;
}

/**
 * Decide whether to upload a frame using dirty rects, or fall back to a
 * full-frame upload.
 *
 * `null` means "upload the full frame".
 * `[]` means "upload nothing" (callers may still treat this as full-frame,
 * depending on existing semantics).
 */
export function chooseDirtyRectsForUpload(
  layout: SharedFramebufferLayout,
  rects: DirtyRect[] | null,
  bytesPerRowAlignment: number,
  opts?: DirtyRectUploadPolicyOptions,
): DirtyRect[] | null {
  if (rects == null) return null;
  if (rects.length === 0) return [];

  const maxRects = opts?.maxRects ?? DEFAULT_MAX_RECTS;
  if (rects.length > maxRects) return null;

  const fullFrameRatioThreshold = opts?.fullFrameRatioThreshold ?? DEFAULT_FULL_FRAME_RATIO_THRESHOLD;

  const dirtyBytes = estimateTextureUploadBytes(layout, rects, bytesPerRowAlignment);
  const fullBytes = estimateFullFrameUploadBytes(layout.width, layout.height, bytesPerRowAlignment);

  if (dirtyBytes >= fullBytes * fullFrameRatioThreshold) return null;

  return rects;
}

