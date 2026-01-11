import { describe, expect, it, vi } from "vitest";

import {
  WebUsbBackend,
  dataViewToUint8Array,
  executeWebUsbControlIn,
  parseBmRequestType,
  validateControlTransferDirection,
} from "./webusb_backend";

function withFakeNavigatorUsb<T>(fn: () => Promise<T>): Promise<T> {
  const nav = globalThis.navigator as unknown as Record<string, unknown>;
  const originalUsbDescriptor = Object.getOwnPropertyDescriptor(nav, "usb");

  Object.defineProperty(nav, "usb", { value: {}, configurable: true });

  return fn().finally(() => {
    if (originalUsbDescriptor) {
      Object.defineProperty(nav, "usb", originalUsbDescriptor);
    } else {
      delete (nav as { usb?: unknown }).usb;
    }
  });
}

function dataViewFromBytes(bytes: number[]): DataView {
  return new DataView(Uint8Array.from(bytes).buffer);
}

describe("webusb_backend helpers", () => {
  it("maps bmRequestType to WebUSB {requestType, recipient}", () => {
    expect(parseBmRequestType(0x80)).toMatchObject({ requestType: "standard", recipient: "device" });
    expect(parseBmRequestType(0x21)).toMatchObject({ requestType: "class", recipient: "interface" });
    expect(parseBmRequestType(0xc2)).toMatchObject({ requestType: "vendor", recipient: "endpoint" });
    expect(parseBmRequestType(0xa3)).toMatchObject({ requestType: "class", recipient: "other" });
  });

  it("validates control transfer direction against bmRequestType", () => {
    expect(validateControlTransferDirection("controlIn", 0x80).ok).toBe(true);
    expect(validateControlTransferDirection("controlOut", 0x00).ok).toBe(true);

    const wrongIn = validateControlTransferDirection("controlIn", 0x00);
    expect(wrongIn.ok).toBe(false);
    if (!wrongIn.ok) expect(wrongIn.message).toContain("expected deviceToHost");

    const wrongOut = validateControlTransferDirection("controlOut", 0x80);
    expect(wrongOut.ok).toBe(false);
    if (!wrongOut.ok) expect(wrongOut.message).toContain("expected hostToDevice");
  });

  it("converts DataView to a trimmed Uint8Array copy", () => {
    const buf = Uint8Array.from([0xaa, 0xbb, 0xcc, 0xdd]).buffer;
    const view = new DataView(buf, 1, 2); // [0xbb, 0xcc]

    const out = dataViewToUint8Array(view);
    expect(Array.from(out)).toEqual([0xbb, 0xcc]);
    expect(out.byteLength).toBe(2);
  });
});

describe("executeWebUsbControlIn", () => {
  it("translates OTHER_SPEED_CONFIGURATION to CONFIGURATION for full-speed guests", async () => {
    const controlTransferIn = vi.fn<[USBControlTransferParameters, number], Promise<USBInTransferResult>>();
    controlTransferIn.mockResolvedValueOnce({
      status: "ok",
      data: dataViewFromBytes([0x09, 0x07, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]),
    });

    const res = await executeWebUsbControlIn(
      { controlTransferIn },
      {
        bmRequestType: 0x80,
        bRequest: 0x06,
        wValue: 0x0200,
        wIndex: 0x0000,
        wLength: 9,
      },
    );

    expect(res.status).toBe("ok");
    if (res.status !== "ok") throw new Error("unreachable");
    expect(Array.from(res.data)).toEqual([0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]);

    expect(controlTransferIn).toHaveBeenCalledTimes(1);
    expect(controlTransferIn.mock.calls[0]?.[0].value).toBe(0x0700);
    expect(controlTransferIn.mock.calls[0]?.[1]).toBe(9);
  });

  it("falls back to CONFIGURATION when OTHER_SPEED_CONFIGURATION stalls", async () => {
    const controlTransferIn = vi.fn<[USBControlTransferParameters, number], Promise<USBInTransferResult>>();
    controlTransferIn.mockResolvedValueOnce({ status: "stall" });
    controlTransferIn.mockResolvedValueOnce({
      status: "ok",
      data: dataViewFromBytes([0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]),
    });

    const res = await executeWebUsbControlIn(
      { controlTransferIn },
      {
        bmRequestType: 0x80,
        bRequest: 0x06,
        wValue: 0x0200,
        wIndex: 0x0000,
        wLength: 9,
      },
    );

    expect(res.status).toBe("ok");
    if (res.status !== "ok") throw new Error("unreachable");
    expect(Array.from(res.data)).toEqual([0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]);

    expect(controlTransferIn).toHaveBeenCalledTimes(2);
    expect(controlTransferIn.mock.calls[0]?.[0].value).toBe(0x0700);
    expect(controlTransferIn.mock.calls[1]?.[0].value).toBe(0x0200);
  });
});

describe("WebUsbBackend.ensureOpenAndClaimed", () => {
  it("claims available interfaces but tolerates protected/failed claims", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: false, alternates: [], alternate: {} };
      const iface2 = { interfaceNumber: 2, claimed: false, alternates: [], alternate: {} };
      const config = { configurationValue: 1, interfaces: [iface1, iface2] };

      const device: Partial<USBDevice> = {
        vendorId: 0x1234,
        productId: 0x5678,
        opened: false,
        configuration: null,
        configurations: [config as unknown as USBConfiguration],
        open: vi.fn(async () => {
          (device as any).opened = true;
        }),
        selectConfiguration: vi.fn(async (value: number) => {
          expect(value).toBe(1);
          (device as any).configuration = config;
        }),
        claimInterface: vi.fn(async (ifaceNum: number) => {
          if (ifaceNum === 1) {
            throw new Error("protected interface");
          }
          if (ifaceNum === 2) {
            iface2.claimed = true;
            return;
          }
          throw new Error(`unexpected iface ${ifaceNum}`);
        }),
      };

      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      try {
        const backend = new WebUsbBackend(device as USBDevice);
        await backend.ensureOpenAndClaimed();
        expect((device.open as any).mock.calls.length).toBe(1);
        expect((device.selectConfiguration as any).mock.calls.length).toBe(1);
        expect((device.claimInterface as any).mock.calls.length).toBe(2);
      } finally {
        warn.mockRestore();
      }
    });
  });

  it("throws when no interfaces can be claimed", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: false, alternates: [], alternate: {} };
      const config = { configurationValue: 1, interfaces: [iface1] };

      const device: Partial<USBDevice> = {
        vendorId: 0x1234,
        productId: 0x5678,
        opened: true,
        configuration: config as unknown as USBConfiguration,
        configurations: [config as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {
          throw new Error("nope");
        }),
      };

      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      try {
        const backend = new WebUsbBackend(device as USBDevice);
        await expect(backend.ensureOpenAndClaimed()).rejects.toThrow("Failed to claim any USB interface");
      } finally {
        warn.mockRestore();
      }
    });
  });

  it("retries claim when cached state is stale (interface no longer claimed)", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: false, alternates: [], alternate: {} };
      const config = { configurationValue: 1, interfaces: [iface1] };

      const device: Partial<USBDevice> = {
        vendorId: 0x1234,
        productId: 0x5678,
        opened: true,
        configuration: config as unknown as USBConfiguration,
        configurations: [config as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {
          iface1.claimed = true;
        }),
      };

      const backend = new WebUsbBackend(device as USBDevice);
      await backend.ensureOpenAndClaimed();
      expect((device.claimInterface as any).mock.calls.length).toBe(1);

      // Simulate the device being closed/reopened or the claim being lost.
      iface1.claimed = false;
      await backend.ensureOpenAndClaimed();
      expect((device.claimInterface as any).mock.calls.length).toBe(2);
    });
  });
});

describe("WebUsbBackend.execute controlOut translations", () => {
  it("translates SET_CONFIGURATION into device.selectConfiguration", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: true, alternates: [], alternate: {} };
      const config1 = { configurationValue: 1, interfaces: [iface1] };
      const config2 = { configurationValue: 2, interfaces: [iface1] };

      const device: Partial<USBDevice> = {
        opened: true,
        configuration: config1 as unknown as USBConfiguration,
        configurations: [config1 as unknown as USBConfiguration, config2 as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {}),
        releaseInterface: vi.fn(async (ifaceNum: number) => {
          expect(ifaceNum).toBe(1);
          iface1.claimed = false;
        }),
        selectConfiguration: vi.fn(async (value: number) => {
          expect(value).toBe(2);
          (device as any).configuration = config2;
        }),
        controlTransferOut: vi.fn(async () => {
          throw new Error("controlTransferOut should not be called");
        }),
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const res = await backend.execute({
        kind: "controlOut",
        id: 1,
        setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 0x0002, wIndex: 0x0000, wLength: 0x0000 },
        data: new Uint8Array(),
      });

      expect(device.releaseInterface).toHaveBeenCalledTimes(1);
      expect(device.selectConfiguration).toHaveBeenCalledTimes(1);
      expect(device.controlTransferOut).not.toHaveBeenCalled();
      expect(res).toEqual({ kind: "controlOut", id: 1, status: "success", bytesWritten: 0 });
    });
  });

  it("translates SET_CONFIGURATION even when interfaces cannot be claimed", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: false, alternates: [], alternate: {} };
      const config1 = { configurationValue: 1, interfaces: [iface1] };
      const config2 = { configurationValue: 2, interfaces: [iface1] };

      const device: Partial<USBDevice> = {
        opened: true,
        configuration: config1 as unknown as USBConfiguration,
        configurations: [config1 as unknown as USBConfiguration, config2 as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {
          throw new Error("claimInterface should not be called");
        }),
        releaseInterface: vi.fn(async () => {
          throw new Error("releaseInterface should not be called");
        }),
        selectConfiguration: vi.fn(async (value: number) => {
          expect(value).toBe(2);
          (device as any).configuration = config2;
        }),
        controlTransferOut: vi.fn(async () => {
          throw new Error("controlTransferOut should not be called");
        }),
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const res = await backend.execute({
        kind: "controlOut",
        id: 99,
        setup: { bmRequestType: 0x00, bRequest: 0x09, wValue: 0x0002, wIndex: 0x0000, wLength: 0x0000 },
        data: new Uint8Array(),
      });

      expect(device.selectConfiguration).toHaveBeenCalledTimes(1);
      expect(device.claimInterface).not.toHaveBeenCalled();
      expect(device.releaseInterface).not.toHaveBeenCalled();
      expect(device.controlTransferOut).not.toHaveBeenCalled();
      expect(res).toEqual({ kind: "controlOut", id: 99, status: "success", bytesWritten: 0 });
    });
  });

  it("translates SET_INTERFACE into device.selectAlternateInterface", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface3 = { interfaceNumber: 3, claimed: true, alternates: [], alternate: {} };
      const config = { configurationValue: 1, interfaces: [iface3] };

      const device: Partial<USBDevice> = {
        opened: true,
        configuration: config as unknown as USBConfiguration,
        configurations: [config as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        selectAlternateInterface: vi.fn(async (ifaceNum: number, altSetting: number) => {
          expect(ifaceNum).toBe(3);
          expect(altSetting).toBe(2);
        }),
        controlTransferOut: vi.fn(async () => {
          throw new Error("controlTransferOut should not be called");
        }),
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const res = await backend.execute({
        kind: "controlOut",
        id: 2,
        setup: { bmRequestType: 0x01, bRequest: 0x0b, wValue: 0x0002, wIndex: 0x0003, wLength: 0x0000 },
        data: new Uint8Array(),
      });

      expect(device.selectAlternateInterface).toHaveBeenCalledTimes(1);
      expect(device.controlTransferOut).not.toHaveBeenCalled();
      expect(res).toEqual({ kind: "controlOut", id: 2, status: "success", bytesWritten: 0 });
    });
  });

  it("translates CLEAR_FEATURE(ENDPOINT_HALT) into device.clearHalt", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: true, alternates: [], alternate: {} };
      const config = { configurationValue: 1, interfaces: [iface1] };

      const device: Partial<USBDevice> = {
        opened: true,
        configuration: config as unknown as USBConfiguration,
        configurations: [config as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        clearHalt: vi.fn(async (direction: USBDirection, endpointNumber: number) => {
          expect(direction).toBe("in");
          expect(endpointNumber).toBe(1);
        }),
        controlTransferOut: vi.fn(async () => {
          throw new Error("controlTransferOut should not be called");
        }),
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const res = await backend.execute({
        kind: "controlOut",
        id: 3,
        setup: { bmRequestType: 0x02, bRequest: 0x01, wValue: 0x0000, wIndex: 0x0081, wLength: 0x0000 },
        data: new Uint8Array(),
      });

      expect(device.clearHalt).toHaveBeenCalledTimes(1);
      expect(device.controlTransferOut).not.toHaveBeenCalled();
      expect(res).toEqual({ kind: "controlOut", id: 3, status: "success", bytesWritten: 0 });
    });
  });
});
