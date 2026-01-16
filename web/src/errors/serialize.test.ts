import { describe, expect, test } from "vitest";

import {
  DEFAULT_ERROR_BYTE_LIMITS,
  serializeErrorForProtocol,
  serializeErrorForWorker,
} from "./serialize";

describe("errors/serialize", () => {
  test("serializeErrorForWorker returns message-only for non-error values", () => {
    expect(serializeErrorForWorker(123)).toEqual({ message: "123" });
    expect(serializeErrorForWorker(null)).toEqual({ message: "null" });
  });

  test("serializeErrorForWorker returns bounded name/message/stack for Error", () => {
    const err = new Error("a\tb\nc");
    err.name = "MyError";
    err.stack = "line1\nline2\nline3";

    expect(serializeErrorForWorker(err)).toEqual({
      name: "MyError",
      message: "a b c",
      stack: "line1\nline2\nline3",
    });
  });

  test("serializeErrorForProtocol always returns a name", () => {
    expect(serializeErrorForProtocol("boom")).toEqual({ name: "Error", message: "boom" });
  });

  test("serialization respects byte caps (UTF-8)", () => {
    const limits = { ...DEFAULT_ERROR_BYTE_LIMITS, maxMessageBytes: 4, maxStackBytes: 0 };

    const err = new Error("🙂🙂");
    const worker = serializeErrorForWorker(err, limits);
    expect(worker.name).toBe("Error");
    expect(worker.message).toBe("🙂");
    expect(worker.stack).toBeUndefined();

    const protocol = serializeErrorForProtocol(err, limits);
    expect(protocol.name).toBe("Error");
    expect(protocol.message).toBe("🙂");
    expect(protocol.stack).toBeUndefined();
  });

  test("serializeErrorForProtocol accepts Error-like objects", () => {
    const obj = { name: "X", message: "y\nz", stack: "s" };
    expect(serializeErrorForProtocol(obj)).toEqual({ name: "X", message: "y z", stack: "s" });
  });
});

