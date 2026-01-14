import { describe, expect, it } from "vitest";

import { I32_MAX, I32_MIN, negateI32Saturating } from "./int32";

describe("input/int32.negateI32Saturating", () => {
  it("negates normal values", () => {
    expect(negateI32Saturating(0)).toBe(0);
    expect(negateI32Saturating(123)).toBe(-123);
    expect(negateI32Saturating(-456)).toBe(456);
  });

  it("saturates i32::MIN to i32::MAX", () => {
    expect(negateI32Saturating(I32_MIN)).toBe(I32_MAX);
  });
});

