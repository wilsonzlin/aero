import { describe, expect, it } from "vitest";

import { I32_MAX, I32_MIN, addI32Saturating, negateI32Saturating } from "./int32";

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

describe("input/int32.addI32Saturating", () => {
  it("adds normal values", () => {
    expect(addI32Saturating(0, 0)).toBe(0);
    expect(addI32Saturating(123, 456)).toBe(579);
    expect(addI32Saturating(-123, 456)).toBe(333);
  });

  it("saturates overflows", () => {
    expect(addI32Saturating(I32_MAX, 1)).toBe(I32_MAX);
    expect(addI32Saturating(I32_MIN, -1)).toBe(I32_MIN);
  });
});
