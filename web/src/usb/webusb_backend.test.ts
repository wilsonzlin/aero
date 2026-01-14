import { describe, expect, it, vi } from "vitest";

import { isWebUsbProtectedInterfaceClass } from "../platform/webusb";

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

describe("webusb_backend executeWebUsbControlIn", () => {
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
      { translateOtherSpeedConfigurationDescriptor: true },
    );

    expect(res.status).toBe("ok");
    if (res.status !== "ok") throw new Error("unreachable");
    expect(Array.from(res.data)).toEqual([0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]);

    expect(controlTransferIn).toHaveBeenCalledTimes(1);
    expect(controlTransferIn.mock.calls[0]?.[0].value).toBe(0x0700);
    expect(controlTransferIn.mock.calls[0]?.[1]).toBe(9);
  });

  it("can disable OTHER_SPEED_CONFIGURATION translation (EHCI/high-speed guests)", async () => {
    const controlTransferIn = vi.fn<[USBControlTransferParameters, number], Promise<USBInTransferResult>>();
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
      { translateOtherSpeedConfigurationDescriptor: false },
    );

    expect(res.status).toBe("ok");
    if (res.status !== "ok") throw new Error("unreachable");
    expect(Array.from(res.data)).toEqual([0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]);

    // Must not attempt the OTHER_SPEED_CONFIGURATION fetch.
    expect(controlTransferIn).toHaveBeenCalledTimes(1);
    expect(controlTransferIn.mock.calls[0]?.[0].value).toBe(0x0200);
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
      { translateOtherSpeedConfigurationDescriptor: true },
    );

    expect(res.status).toBe("ok");
    if (res.status !== "ok") throw new Error("unreachable");
    expect(Array.from(res.data)).toEqual([0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]);

    expect(controlTransferIn).toHaveBeenCalledTimes(2);
    expect(controlTransferIn.mock.calls[0]?.[0].value).toBe(0x0700);
    expect(controlTransferIn.mock.calls[1]?.[0].value).toBe(0x0200);
  });

  it("does not translate CONFIGURATION to OTHER_SPEED_CONFIGURATION when disabled", async () => {
    const controlTransferIn = vi.fn<[USBControlTransferParameters, number], Promise<USBInTransferResult>>();
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
      { translateOtherSpeedConfigurationDescriptor: false },
    );

    expect(res.status).toBe("ok");
    if (res.status !== "ok") throw new Error("unreachable");
    expect(Array.from(res.data)).toEqual([0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 50]);

    expect(controlTransferIn).toHaveBeenCalledTimes(1);
    expect(controlTransferIn.mock.calls[0]?.[0].value).toBe(0x0200);
    expect(controlTransferIn.mock.calls[0]?.[1]).toBe(9);
  });
});

describe("webusb_backend WebUsbBackend.ensureOpenAndClaimed", () => {
  it("claims available interfaces but tolerates protected/failed claims", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: false, alternates: [], alternate: {} };
      const iface2 = { interfaceNumber: 2, claimed: false, alternates: [], alternate: {} };
      const config = { configurationValue: 1, interfaces: [iface1, iface2] };

      let opened = false;
      let configuration: USBConfiguration | null = null;
      const open = vi.fn(async () => {
        opened = true;
      });
      const selectConfiguration = vi.fn(async (value: number) => {
        expect(value).toBe(1);
        configuration = config as unknown as USBConfiguration;
      });
      const claimInterface = vi.fn(async (ifaceNum: number) => {
        if (ifaceNum === 1) {
          throw new Error("protected interface");
        }
        if (ifaceNum === 2) {
          iface2.claimed = true;
          return;
        }
        throw new Error(`unexpected iface ${ifaceNum}`);
      });

      const device: Partial<USBDevice> = {
        vendorId: 0x1234,
        productId: 0x5678,
        get opened() {
          return opened;
        },
        get configuration() {
          return configuration;
        },
        configurations: [config as unknown as USBConfiguration],
        open,
        selectConfiguration,
        claimInterface,
      };

      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      try {
        const backend = new WebUsbBackend(device as USBDevice);
        await backend.ensureOpenAndClaimed();
        expect(open).toHaveBeenCalledTimes(1);
        expect(selectConfiguration).toHaveBeenCalledTimes(1);
        expect(claimInterface).toHaveBeenCalledTimes(2);
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

  it("throws when all interfaces are protected by Chromium WebUSB restrictions", async () => {
    await withFakeNavigatorUsb(async () => {
      const hidClass = 0x03;
      expect(isWebUsbProtectedInterfaceClass(hidClass)).toBe(true);

      const hidAlternate = { alternateSetting: 0, interfaceClass: hidClass, interfaceSubclass: 0, interfaceProtocol: 0 };
      const iface1 = { interfaceNumber: 1, claimed: false, alternates: [hidAlternate], alternate: hidAlternate };
      const config = { configurationValue: 1, interfaces: [iface1] };

      const claimInterface = vi.fn(async () => {
        throw new Error("claimInterface should not be called for protected interfaces");
      });
      const device: Partial<USBDevice> = {
        vendorId: 0x1234,
        productId: 0x5678,
        opened: true,
        configuration: config as unknown as USBConfiguration,
        configurations: [config as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        claimInterface,
      };

      const backend = new WebUsbBackend(device as USBDevice);
      await expect(backend.ensureOpenAndClaimed()).rejects.toThrow(
        /all interfaces are protected by Chromium WebUSB restrictions.*0x1234:0x5678/i,
      );
      expect(claimInterface).not.toHaveBeenCalled();
    });
  });

  it("retries claim when cached state is stale (interface no longer claimed)", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: false, alternates: [], alternate: {} };
      const config = { configurationValue: 1, interfaces: [iface1] };

      const claimInterface = vi.fn(async () => {
        iface1.claimed = true;
      });
      const device: Partial<USBDevice> = {
        vendorId: 0x1234,
        productId: 0x5678,
        opened: true,
        configuration: config as unknown as USBConfiguration,
        configurations: [config as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        claimInterface,
      };

      const backend = new WebUsbBackend(device as USBDevice);
      await backend.ensureOpenAndClaimed();
      expect(claimInterface).toHaveBeenCalledTimes(1);

      // Simulate the device being closed/reopened or the claim being lost.
      iface1.claimed = false;
      await backend.ensureOpenAndClaimed();
      expect(claimInterface).toHaveBeenCalledTimes(2);
    });
  });
});

describe("webusb_backend WebUsbBackend.execute controlOut translations", () => {
  it("translates SET_CONFIGURATION into device.selectConfiguration", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: true, alternates: [], alternate: {} };
      const config1 = { configurationValue: 1, interfaces: [iface1] };
      const config2 = { configurationValue: 2, interfaces: [iface1] };

      let configuration: USBConfiguration | null = config1 as unknown as USBConfiguration;
      const device: Partial<USBDevice> = {
        opened: true,
        get configuration() {
          return configuration;
        },
        configurations: [config1 as unknown as USBConfiguration, config2 as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {}),
        releaseInterface: vi.fn(async (ifaceNum: number) => {
          expect(ifaceNum).toBe(1);
          iface1.claimed = false;
        }),
        selectConfiguration: vi.fn(async (value: number) => {
          expect(value).toBe(2);
          configuration = config2 as unknown as USBConfiguration;
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

      let configuration: USBConfiguration | null = config1 as unknown as USBConfiguration;
      const device: Partial<USBDevice> = {
        opened: true,
        get configuration() {
          return configuration;
        },
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
          configuration = config2 as unknown as USBConfiguration;
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

  it("omits the data argument for zero-length controlTransferOut payloads", async () => {
    await withFakeNavigatorUsb(async () => {
      const iface1 = { interfaceNumber: 1, claimed: true, alternates: [], alternate: {} };
      const config = { configurationValue: 1, interfaces: [iface1] };

      const controlTransferOut = vi.fn<[USBControlTransferParameters, BufferSource?], Promise<USBOutTransferResult>>();
      controlTransferOut.mockResolvedValueOnce({ status: "ok", bytesWritten: 0 });

      const device: Partial<USBDevice> = {
        opened: true,
        configuration: config as unknown as USBConfiguration,
        configurations: [config as unknown as USBConfiguration],
        open: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        controlTransferOut,
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const res = await backend.execute({
        kind: "controlOut",
        id: 10,
        setup: { bmRequestType: 0x40, bRequest: 0x01, wValue: 0x0000, wIndex: 0x0000, wLength: 0x0000 },
        data: new Uint8Array(),
      });

      expect(controlTransferOut).toHaveBeenCalledTimes(1);
      expect(controlTransferOut.mock.calls[0]?.length).toBe(1);
      expect(res).toEqual({ kind: "controlOut", id: 10, status: "success", bytesWritten: 0 });
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

describe("WebUsbBackend.execute bulk endpoint validation", () => {
  it("rejects bulkIn actions with an OUT endpoint address", async () => {
    await withFakeNavigatorUsb(async () => {
      const transferIn = vi.fn<[number, number], Promise<USBInTransferResult>>();
      transferIn.mockResolvedValueOnce({ status: "ok", data: dataViewFromBytes([0x00]) });

      const transferOut = vi.fn<[number, BufferSource], Promise<USBOutTransferResult>>();
      transferOut.mockResolvedValueOnce({ status: "ok", bytesWritten: 1 });

      const open = vi.fn(async () => {});
      const selectConfiguration = vi.fn(async () => {});
      const claimInterface = vi.fn(async () => {});

      const device: Partial<USBDevice> = {
        opened: false,
        configuration: null,
        configurations: [],
        open,
        selectConfiguration,
        claimInterface,
        transferIn,
        transferOut,
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const res = await backend.execute({ kind: "bulkIn", id: 1, endpoint: 0x02, length: 1 });
      expect(res.kind).toBe("bulkIn");
      expect(res.status).toBe("error");
      expect(transferIn).not.toHaveBeenCalled();
      expect(open).not.toHaveBeenCalled();
      expect(selectConfiguration).not.toHaveBeenCalled();
      expect(claimInterface).not.toHaveBeenCalled();
    });
  });

  it("rejects bulkOut actions with an IN endpoint address", async () => {
    await withFakeNavigatorUsb(async () => {
      const transferIn = vi.fn<[number, number], Promise<USBInTransferResult>>();
      transferIn.mockResolvedValueOnce({ status: "ok", data: dataViewFromBytes([0x00]) });

      const transferOut = vi.fn<[number, BufferSource], Promise<USBOutTransferResult>>();
      transferOut.mockResolvedValueOnce({ status: "ok", bytesWritten: 1 });

      const open = vi.fn(async () => {});
      const selectConfiguration = vi.fn(async () => {});
      const claimInterface = vi.fn(async () => {});

      const device: Partial<USBDevice> = {
        opened: false,
        configuration: null,
        configurations: [],
        open,
        selectConfiguration,
        claimInterface,
        transferIn,
        transferOut,
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const res = await backend.execute({ kind: "bulkOut", id: 2, endpoint: 0x81, data: new Uint8Array([1]) });
      expect(res).toMatchObject({ kind: "bulkOut", id: 2, status: "error" });
      expect(transferOut).not.toHaveBeenCalled();
      expect(open).not.toHaveBeenCalled();
      expect(selectConfiguration).not.toHaveBeenCalled();
      expect(claimInterface).not.toHaveBeenCalled();
    });
  });

  it("rejects endpoint 0 for bulkIn and bulkOut", async () => {
    await withFakeNavigatorUsb(async () => {
      const transferIn = vi.fn<[number, number], Promise<USBInTransferResult>>();
      transferIn.mockResolvedValueOnce({ status: "ok", data: dataViewFromBytes([0x00]) });

      const transferOut = vi.fn<[number, BufferSource], Promise<USBOutTransferResult>>();
      transferOut.mockResolvedValueOnce({ status: "ok", bytesWritten: 1 });

      const device: Partial<USBDevice> = {
        opened: true,
        configuration: null,
        configurations: [],
        open: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {}),
        transferIn,
        transferOut,
      };

      const backend = new WebUsbBackend(device as USBDevice);

      const inRes = await backend.execute({ kind: "bulkIn", id: 3, endpoint: 0x80, length: 1 });
      expect(inRes).toMatchObject({ kind: "bulkIn", id: 3, status: "error" });
      expect(transferIn).not.toHaveBeenCalled();

      const outRes = await backend.execute({ kind: "bulkOut", id: 4, endpoint: 0x00, data: new Uint8Array([1]) });
      expect(outRes).toMatchObject({ kind: "bulkOut", id: 4, status: "error" });
      expect(transferOut).not.toHaveBeenCalled();
    });
  });

  it("rejects bulkIn actions with lengths above the safety cap", async () => {
    await withFakeNavigatorUsb(async () => {
      const transferIn = vi.fn<[number, number], Promise<USBInTransferResult>>();
      transferIn.mockResolvedValueOnce({ status: "ok", data: dataViewFromBytes([0x00]) });

      const device: Partial<USBDevice> = {
        opened: true,
        configuration: null,
        configurations: [],
        open: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {}),
        transferIn,
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const tooLarge = 4 * 1024 * 1024 + 1;
      const res = await backend.execute({ kind: "bulkIn", id: 5, endpoint: 0x81, length: tooLarge });
      expect(res).toMatchObject({ kind: "bulkIn", id: 5, status: "error" });
      expect(transferIn).not.toHaveBeenCalled();
    });
  });

  it("rejects bulkOut payloads above the safety cap", async () => {
    await withFakeNavigatorUsb(async () => {
      const transferOut = vi.fn<[number, BufferSource], Promise<USBOutTransferResult>>();
      transferOut.mockResolvedValueOnce({ status: "ok", bytesWritten: 0 });

      const device: Partial<USBDevice> = {
        opened: true,
        configuration: null,
        configurations: [],
        open: vi.fn(async () => {}),
        selectConfiguration: vi.fn(async () => {}),
        claimInterface: vi.fn(async () => {}),
        transferOut,
      };

      const backend = new WebUsbBackend(device as USBDevice);
      const tooLarge = new Uint8Array(4 * 1024 * 1024 + 1);
      const res = await backend.execute({ kind: "bulkOut", id: 6, endpoint: 0x02, data: tooLarge });
      expect(res).toMatchObject({ kind: "bulkOut", id: 6, status: "error" });
      expect(transferOut).not.toHaveBeenCalled();
    });
  });
});
