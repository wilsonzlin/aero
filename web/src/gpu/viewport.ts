import type { PresenterScaleMode } from './presenter';

export type Viewport = { x: number; y: number; w: number; h: number };

export function computeViewport(
  canvasWidthPx: number,
  canvasHeightPx: number,
  srcWidth: number,
  srcHeight: number,
  mode: PresenterScaleMode,
): Viewport {
  if (canvasWidthPx <= 0 || canvasHeightPx <= 0 || srcWidth <= 0 || srcHeight <= 0) {
    return { x: 0, y: 0, w: 0, h: 0 };
  }

  if (mode === 'stretch') {
    return { x: 0, y: 0, w: canvasWidthPx, h: canvasHeightPx };
  }

  const scaleFit = Math.min(canvasWidthPx / srcWidth, canvasHeightPx / srcHeight);
  let scale = scaleFit;

  if (mode === 'integer') {
    const integerScale = Math.floor(scaleFit);
    scale = integerScale >= 1 ? integerScale : scaleFit;
  }

  const w = Math.max(1, Math.floor(srcWidth * scale));
  const h = Math.max(1, Math.floor(srcHeight * scale));
  const x = Math.floor((canvasWidthPx - w) / 2);
  const y = Math.floor((canvasHeightPx - h) / 2);
  return { x, y, w, h };
}
