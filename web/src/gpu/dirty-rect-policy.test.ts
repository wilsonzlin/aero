import { describe, expect, it } from "vitest";

import { chooseDirtyRectsForUpload } from "./dirty-rect-policy";
import { computeSharedFramebufferLayout, FramebufferFormat, type DirtyRect } from "../ipc/shared-layout";

describe("chooseDirtyRectsForUpload", () => {
  it("forces a full-frame upload when the rect list is extremely large", () => {
    const layout = computeSharedFramebufferLayout(256, 256, 256 * 4, FramebufferFormat.RGBA8, 32);
    const rects: DirtyRect[] = Array.from({ length: 2000 }, (_, i) => ({
      x: i % 256,
      y: (i / 256) | 0,
      w: 1,
      h: 1,
    }));

    const chosen = chooseDirtyRectsForUpload(layout, rects, 256);
    expect(chosen).toBeNull();
  });

  it("forces a full-frame upload when dirty rect uploads approach full-frame bandwidth", () => {
    const layout = computeSharedFramebufferLayout(100, 100, 100 * 4, FramebufferFormat.RGBA8, 32);
    const rects: DirtyRect[] = [{ x: 0, y: 0, w: 80, h: 100 }];

    const chosen = chooseDirtyRectsForUpload(layout, rects, 256);
    expect(chosen).toBeNull();
  });

  it("keeps typical small dirty rect sets unchanged", () => {
    const layout = computeSharedFramebufferLayout(100, 100, 100 * 4, FramebufferFormat.RGBA8, 32);
    const rects: DirtyRect[] = [{ x: 10, y: 20, w: 10, h: 10 }];

    const chosen = chooseDirtyRectsForUpload(layout, rects, 256);
    expect(chosen).toBe(rects);
  });
});

