import { describe, expect, it } from "vitest";

import type { Presenter } from "../gpu/presenter";
import { didPresenterPresent } from "./present-outcome";

describe("gpu-worker present outcome handling", () => {
  it("treats `false` as dropped and `undefined` as presented (back-compat)", () => {
    expect(didPresenterPresent(false)).toBe(false);
    expect(didPresenterPresent(true)).toBe(true);
    expect(didPresenterPresent(undefined)).toBe(true);
  });

  it("propagates a presenter returning false as a drop", () => {
    const fakePresenter: Presenter = {
      backend: "webgpu",
      init() {},
      resize() {},
      present() {
        return false;
      },
      screenshot() {
        return { width: 0, height: 0, pixels: new ArrayBuffer(0) };
      },
    };

    const didPresent = didPresenterPresent(fakePresenter.present(new Uint8Array(4), 4));
    expect(didPresent).toBe(false);
  });
});

