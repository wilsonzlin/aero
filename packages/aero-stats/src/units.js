export function frameTimeMsToFps(frameTimeMs) {
  if (!Number.isFinite(frameTimeMs)) return Number.NaN;
  if (frameTimeMs <= 0) return Number.POSITIVE_INFINITY;
  return 1000 / frameTimeMs;
}

export function fpsToFrameTimeMs(fps) {
  if (!Number.isFinite(fps)) return Number.NaN;
  if (fps <= 0) return Number.POSITIVE_INFINITY;
  return 1000 / fps;
}

export function msToUs(ms) {
  if (!Number.isFinite(ms)) return Number.NaN;
  return ms * 1000;
}

export function usToMs(us) {
  if (!Number.isFinite(us)) return Number.NaN;
  return us / 1000;
}

export function computeMips({ instructions, elapsedMs }) {
  if (!Number.isFinite(instructions) || !Number.isFinite(elapsedMs)) {
    throw new TypeError('computeMips: inputs must be finite');
  }
  if (elapsedMs <= 0) return Number.NaN;
  return instructions / (elapsedMs / 1000) / 1e6;
}

