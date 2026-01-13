import { describe, expect, it } from "vitest";

import { STATUS_BYTES, StatusIndex } from "../runtime/shared_layout";
import {
  INPUT_BACKEND_STATUS_CODE,
  decodeInputBackendStatus,
  encodeInputBackendStatus,
  type InputBackendStatusCode,
} from "./input_backend_status";
import type { InputBackend } from "./input_backend_selection";

describe("input/input_backend_status", () => {
  it("encodes and decodes backend status codes", () => {
    const cases: Array<{ backend: InputBackend; code: InputBackendStatusCode }> = [
      { backend: "ps2", code: INPUT_BACKEND_STATUS_CODE.ps2 },
      { backend: "usb", code: INPUT_BACKEND_STATUS_CODE.usb },
      { backend: "virtio", code: INPUT_BACKEND_STATUS_CODE.virtio },
    ];

    for (const c of cases) {
      expect(encodeInputBackendStatus(c.backend)).toBe(c.code);
      expect(decodeInputBackendStatus(c.code)).toBe(c.backend);
    }

    // Unknown codes should not throw (HUD/debug views can display "unknown").
    expect(decodeInputBackendStatus(-1)).toBeNull();
    expect(decodeInputBackendStatus(3)).toBeNull();
    expect(decodeInputBackendStatus(1234)).toBeNull();
  });

  it("round-trips backend codes through the shared runtime status view", () => {
    const status = new Int32Array(new SharedArrayBuffer(STATUS_BYTES));

    Atomics.store(status, StatusIndex.IoInputKeyboardBackend, encodeInputBackendStatus("virtio"));
    Atomics.store(status, StatusIndex.IoInputMouseBackend, encodeInputBackendStatus("usb"));

    const kb = decodeInputBackendStatus(Atomics.load(status, StatusIndex.IoInputKeyboardBackend));
    const mouse = decodeInputBackendStatus(Atomics.load(status, StatusIndex.IoInputMouseBackend));

    expect(kb).toBe("virtio");
    expect(mouse).toBe("usb");
  });
});

