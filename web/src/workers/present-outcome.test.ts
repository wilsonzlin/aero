import { describe, expect, it } from "vitest";

import type { Presenter } from "../gpu/presenter";
import { didPresenterPresent, presentOutcomeDeltas } from "./present-outcome";

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

  it("updates worker counters for dropped vs presented present passes", () => {
    const start = { presentsSucceeded: 10, framesPresented: 20, framesDropped: 30 };

    const drop = presentOutcomeDeltas(false);
    expect(drop).toEqual({ presentsSucceeded: 0, framesPresented: 0, framesDropped: 1 });

    const presented = presentOutcomeDeltas(true);
    expect(presented).toEqual({ presentsSucceeded: 1, framesPresented: 1, framesDropped: 0 });

    const afterDrop = {
      presentsSucceeded: start.presentsSucceeded + drop.presentsSucceeded,
      framesPresented: start.framesPresented + drop.framesPresented,
      framesDropped: start.framesDropped + drop.framesDropped,
    };
    expect(afterDrop).toEqual({ presentsSucceeded: 10, framesPresented: 20, framesDropped: 31 });

    const afterPresented = {
      presentsSucceeded: start.presentsSucceeded + presented.presentsSucceeded,
      framesPresented: start.framesPresented + presented.framesPresented,
      framesDropped: start.framesDropped + presented.framesDropped,
    };
    expect(afterPresented).toEqual({ presentsSucceeded: 11, framesPresented: 21, framesDropped: 30 });
  });
});
