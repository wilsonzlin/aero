import { readFileSync } from "node:fs";

import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsbHostAction, UsbHostCompletion } from "./webusb_backend";
import { WebUsbExecutor } from "./webusb_executor";

const originalNavigatorDescriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");

function stubNavigator(value: unknown): void {
  Object.defineProperty(globalThis, "navigator", {
    value,
    configurable: true,
    enumerable: true,
    writable: true,
  });
}

afterEach(() => {
  if (originalNavigatorDescriptor) {
    Object.defineProperty(globalThis, "navigator", originalNavigatorDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "navigator");
  }
});

function dataViewFromBytes(bytes: number[]): DataView {
  return new DataView(Uint8Array.from(bytes).buffer);
}

function fakeUsbDevice(partial: Partial<USBDevice>): USBDevice {
  const iface0 = {
    interfaceNumber: 0,
    claimed: false,
    alternates: [{ alternateSetting: 0, interfaceClass: 0xff, interfaceSubclass: 0, interfaceProtocol: 0 }],
    alternate: { alternateSetting: 0, interfaceClass: 0xff, interfaceSubclass: 0, interfaceProtocol: 0 },
  };
  const configuration: USBConfiguration = {
    configurationValue: 1,
    configurationName: null,
    interfaces: [iface0 as unknown as USBInterface],
  };

  return {
    vendorId: 0,
    productId: 0,
    opened: true,
    configurations: [configuration],
    configuration,
    open: vi.fn(async () => {}),
    selectConfiguration: vi.fn(async () => {}),
    claimInterface: vi.fn(async (ifaceNum: number) => {
      if (ifaceNum === iface0.interfaceNumber) {
        iface0.claimed = true;
      }
    }),
    controlTransferIn: vi.fn(async () => ({ status: "stall", data: null })),
    controlTransferOut: vi.fn(async () => ({ status: "stall", bytesWritten: 0 })),
    transferIn: vi.fn(async () => ({ status: "stall", data: null })),
    transferOut: vi.fn(async () => ({ status: "stall", bytesWritten: 0 })),
    ...partial,
  } as unknown as USBDevice;
}

describe("WebUsbExecutor", () => {
  it("executes bulk in/out actions and returns completions", async () => {
    stubNavigator({ usb: {} });

    const transferIn = vi.fn<[number, number], Promise<USBInTransferResult>>();
    transferIn.mockResolvedValueOnce({ status: "ok", data: dataViewFromBytes([0x11, 0x22]) });

    const transferOut = vi.fn<[number, BufferSource], Promise<USBOutTransferResult>>();
    transferOut.mockResolvedValueOnce({ status: "ok", bytesWritten: 2 });

    const device = fakeUsbDevice({ transferIn, transferOut });
    const executor = new WebUsbExecutor(device);

    const inCompletion = await executor.execute({ kind: "bulkIn", id: 1, endpoint: 0x81, length: 2 });
    expect(inCompletion.kind).toBe("bulkIn");
    expect(inCompletion.status).toBe("success");
    if (inCompletion.kind !== "bulkIn" || inCompletion.status !== "success") throw new Error("unreachable");
    expect(Array.from(inCompletion.data)).toEqual([0x11, 0x22]);

    expect(transferIn).toHaveBeenCalledTimes(1);
    expect(transferIn).toHaveBeenCalledWith(1, 2);

    const outCompletion = await executor.execute({
      kind: "bulkOut",
      id: 2,
      endpoint: 0x02,
      data: new Uint8Array([9, 8]),
    });
    expect(outCompletion).toEqual({ kind: "bulkOut", id: 2, status: "success", bytesWritten: 2 });

    expect(transferOut).toHaveBeenCalledTimes(1);
    expect(transferOut.mock.calls[0]?.[0]).toBe(2);
    expect(Array.from(transferOut.mock.calls[0]?.[1] as Uint8Array)).toEqual([9, 8]);
  });

  it("maps thrown WebUSB errors to {status: error}", async () => {
    stubNavigator({ usb: {} });

    const transferIn = vi.fn<[number, number], Promise<USBInTransferResult>>();
    transferIn.mockRejectedValueOnce(new Error("boom"));

    const device = fakeUsbDevice({ transferIn });
    const executor = new WebUsbExecutor(device);

    const completion = await executor.execute({ kind: "bulkIn", id: 3, endpoint: 0x81, length: 1 });
    expect(completion.kind).toBe("bulkIn");
    expect(completion.status).toBe("error");
    if (completion.kind !== "bulkIn" || completion.status !== "error") throw new Error("unreachable");
    expect(completion.message).toContain("boom");
  });

  it("keeps OTHER_SPEED_CONFIGURATION → CONFIGURATION translation for controlIn config descriptor requests", async () => {
    stubNavigator({ usb: {} });

    const controlTransferIn = vi.fn<[USBControlTransferParameters, number], Promise<USBInTransferResult>>();
    controlTransferIn.mockResolvedValueOnce({
      status: "ok",
      data: dataViewFromBytes([0x09, 0x07, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]),
    });

    const device = fakeUsbDevice({ controlTransferIn });
    const executor = new WebUsbExecutor(device);

    const completion = await executor.execute({
      kind: "controlIn",
      id: 10,
      setup: {
        bmRequestType: 0x80,
        bRequest: 0x06,
        wValue: 0x0200,
        wIndex: 0x0000,
        wLength: 9,
      },
    });

    expect(completion.kind).toBe("controlIn");
    expect(completion.status).toBe("success");
    if (completion.kind !== "controlIn" || completion.status !== "success") throw new Error("unreachable");

    expect(Array.from(completion.data)).toEqual([0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]);
    expect(controlTransferIn).toHaveBeenCalledTimes(1);
    expect(controlTransferIn.mock.calls[0]?.[0].value).toBe(0x0700);
  });
});

describe("UsbHostAction/Completion wire fixture", () => {
  it("matches the TS union shapes used by the WebUSB backend", () => {
    const url = new URL("../../../docs/fixtures/webusb_passthrough_wire.json", import.meta.url);
    const fixture = JSON.parse(readFileSync(url, "utf-8")) as {
      actions: unknown[];
      completions: unknown[];
    };

    const toUint8Array = (v: unknown): Uint8Array => {
      if (v instanceof Uint8Array) return v;
      if (Array.isArray(v)) return Uint8Array.from(v);
      throw new Error(`expected bytes, got ${typeof v}`);
    };

    const parseAction = (v: unknown): UsbHostAction => {
      if (!v || typeof v !== "object") throw new Error("action must be object");
      const action = v as Record<string, unknown>;
      const kind = action.kind;
      const id = action.id;
      if (typeof kind !== "string" || typeof id !== "number") throw new Error("invalid action header");

      switch (kind) {
        case "controlIn":
          return {
            kind: "controlIn",
            id,
            setup: action.setup as Extract<UsbHostAction, { kind: "controlIn" }>["setup"],
          };
        case "controlOut":
          return {
            kind: "controlOut",
            id,
            setup: action.setup as Extract<UsbHostAction, { kind: "controlOut" }>["setup"],
            data: toUint8Array(action.data),
          };
        case "bulkIn":
          return {
            kind: "bulkIn",
            id,
            endpoint: action.endpoint as number,
            length: action.length as number,
          };
        case "bulkOut":
          return {
            kind: "bulkOut",
            id,
            endpoint: action.endpoint as number,
            data: toUint8Array(action.data),
          };
        default:
          throw new Error(`unknown action kind: ${kind}`);
      }
    };

    const parseCompletion = (v: unknown): UsbHostCompletion => {
      if (!v || typeof v !== "object") throw new Error("completion must be object");
      const c = v as Record<string, unknown>;
      const kind = c.kind;
      const id = c.id;
      const status = c.status;
      if (typeof kind !== "string" || typeof id !== "number" || typeof status !== "string") {
        throw new Error("invalid completion header");
      }

      switch (kind) {
        case "controlIn":
        case "bulkIn":
          if (status === "success") return { kind, id, status: "success", data: toUint8Array(c.data) };
          if (status === "stall") return { kind, id, status: "stall" };
          if (status === "error") return { kind, id, status: "error", message: String(c.message) };
          throw new Error(`unknown status: ${status}`);
        case "controlOut":
        case "bulkOut":
          if (status === "success") return { kind, id, status: "success", bytesWritten: c.bytesWritten as number };
          if (status === "stall") return { kind, id, status: "stall" };
          if (status === "error") return { kind, id, status: "error", message: String(c.message) };
          throw new Error(`unknown status: ${status}`);
        default:
          throw new Error(`unknown completion kind: ${kind}`);
      }
    };

    const toJsonAction = (action: UsbHostAction): unknown => {
      switch (action.kind) {
        case "controlIn":
          return { kind: action.kind, id: action.id, setup: action.setup };
        case "controlOut":
          return { kind: action.kind, id: action.id, setup: action.setup, data: Array.from(action.data) };
        case "bulkIn":
          return { kind: action.kind, id: action.id, endpoint: action.endpoint, length: action.length };
        case "bulkOut":
          return { kind: action.kind, id: action.id, endpoint: action.endpoint, data: Array.from(action.data) };
      }
    };

    const toJsonCompletion = (c: UsbHostCompletion): unknown => {
      switch (c.kind) {
        case "controlIn":
        case "bulkIn":
          if (c.status === "success") return { kind: c.kind, id: c.id, status: c.status, data: Array.from(c.data) };
          if (c.status === "stall") return { kind: c.kind, id: c.id, status: c.status };
          return { kind: c.kind, id: c.id, status: c.status, message: c.message };
        case "controlOut":
        case "bulkOut":
          if (c.status === "success") return { kind: c.kind, id: c.id, status: c.status, bytesWritten: c.bytesWritten };
          if (c.status === "stall") return { kind: c.kind, id: c.id, status: c.status };
          return { kind: c.kind, id: c.id, status: c.status, message: c.message };
      }
    };

    const parsedActions = fixture.actions.map(parseAction);
    const parsedCompletions = fixture.completions.map(parseCompletion);
    expect(parsedActions).toHaveLength(4);
    expect(parsedCompletions).toHaveLength(12);

    expect(parsedActions.map(toJsonAction)).toEqual(fixture.actions);
    expect(parsedCompletions.map(toJsonCompletion)).toEqual(fixture.completions);
  });
});
