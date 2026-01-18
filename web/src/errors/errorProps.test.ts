import { describe, expect, test } from "vitest";

import { isErrorInstance, isInstanceOf, tryGetErrorCause, tryGetErrorCode } from "./errorProps";

describe("errors/errorProps", () => {
  test("tryGetErrorCode returns string codes", () => {
    expect(tryGetErrorCode({ code: "ECONNRESET" })).toBe("ECONNRESET");
    expect(tryGetErrorCode({ code: "" })).toBe("");
  });

  test("tryGetErrorCode returns undefined for non-string codes", () => {
    expect(tryGetErrorCode({ code: 123 })).toBeUndefined();
    expect(tryGetErrorCode({})).toBeUndefined();
    expect(tryGetErrorCode(null)).toBeUndefined();
  });

  test("tryGetErrorCode does not throw on hostile code getter", () => {
    const hostile = {};
    Object.defineProperty(hostile, "code", {
      get() {
        throw new Error("boom");
      },
    });
    expect(() => tryGetErrorCode(hostile)).not.toThrow();
    expect(tryGetErrorCode(hostile)).toBeUndefined();
  });

  test("tryGetErrorCause returns the cause value", () => {
    const cause = new Error("root");
    expect(tryGetErrorCause({ cause })).toBe(cause);
  });

  test("tryGetErrorCause returns undefined when absent", () => {
    expect(tryGetErrorCause({})).toBeUndefined();
    expect(tryGetErrorCause(null)).toBeUndefined();
  });

  test("tryGetErrorCause does not throw on hostile cause getter", () => {
    const hostile = {};
    Object.defineProperty(hostile, "cause", {
      get() {
        throw new Error("boom");
      },
    });
    expect(() => tryGetErrorCause(hostile)).not.toThrow();
    expect(tryGetErrorCause(hostile)).toBeUndefined();
  });

  test("isErrorInstance does not throw when instanceof throws", () => {
    const hostile = new Proxy(
      {},
      {
        getPrototypeOf() {
          throw new Error("boom");
        },
      },
    );
    expect(() => isErrorInstance(hostile)).not.toThrow();
    expect(isErrorInstance(hostile)).toBe(false);
  });

  test("isInstanceOf does not throw when instanceof throws", () => {
    const hostile = new Proxy(
      {},
      {
        getPrototypeOf() {
          throw new Error("boom");
        },
      },
    );
    expect(() => isInstanceOf(hostile, Error)).not.toThrow();
    expect(isInstanceOf(hostile, Error)).toBe(false);
  });
});
