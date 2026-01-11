import { describe, expect, it } from "vitest";

import { classifyWebUsbDevice, describeUsbClassCode, isProtectedInterfaceClass } from "../../../src/platform/webusb_protection";

type MockAlternate = {
  alternateSetting: number;
  interfaceClass: number;
  interfaceSubclass: number;
  interfaceProtocol: number;
};

type MockInterface = {
  interfaceNumber: number;
  alternates: MockAlternate[];
};

type MockConfiguration = {
  configurationValue: number;
  interfaces: MockInterface[];
};

function mockDevice(configurations: MockConfiguration[]): USBDevice {
  return { configurations } as unknown as USBDevice;
}

describe("webusb_protection", () => {
  it("HID-only device has no unprotected interfaces", () => {
    const device = mockDevice([
      {
        configurationValue: 1,
        interfaces: [
          {
            interfaceNumber: 0,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0x03,
                interfaceSubclass: 0x01,
                interfaceProtocol: 0x01,
              },
            ],
          },
        ],
      },
    ]);

    const result = classifyWebUsbDevice(device);
    expect(result.hasAnyInterfaces).toBe(true);
    expect(result.hasUnprotectedInterfaces).toBe(false);
    expect(result.unprotected).toEqual([]);
    expect(result.protected).toEqual([
      {
        configurationValue: 1,
        interfaceNumber: 0,
        alternateSetting: 0,
        classCode: 0x03,
        subclassCode: 0x01,
        protocolCode: 0x01,
      },
    ]);
  });

  it("vendor-specific-only device is requestable/claimable", () => {
    const device = mockDevice([
      {
        configurationValue: 1,
        interfaces: [
          {
            interfaceNumber: 0,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0xff,
                interfaceSubclass: 0x00,
                interfaceProtocol: 0x00,
              },
            ],
          },
        ],
      },
    ]);

    const result = classifyWebUsbDevice(device);
    expect(result.hasAnyInterfaces).toBe(true);
    expect(result.hasUnprotectedInterfaces).toBe(true);
    expect(result.protected).toEqual([]);
    expect(result.unprotected).toEqual([
      {
        configurationValue: 1,
        interfaceNumber: 0,
        alternateSetting: 0,
        classCode: 0xff,
        subclassCode: 0x00,
        protocolCode: 0x00,
      },
    ]);
  });

  it("composite HID + vendor-specific device is requestable and lists both", () => {
    const device = mockDevice([
      {
        configurationValue: 1,
        interfaces: [
          {
            interfaceNumber: 0,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0x03,
                interfaceSubclass: 0x01,
                interfaceProtocol: 0x02,
              },
            ],
          },
          {
            interfaceNumber: 1,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0xff,
                interfaceSubclass: 0x00,
                interfaceProtocol: 0x00,
              },
            ],
          },
        ],
      },
    ]);

    const result = classifyWebUsbDevice(device);
    expect(result.hasAnyInterfaces).toBe(true);
    expect(result.hasUnprotectedInterfaces).toBe(true);
    expect(result.protected).toEqual([
      {
        configurationValue: 1,
        interfaceNumber: 0,
        alternateSetting: 0,
        classCode: 0x03,
        subclassCode: 0x01,
        protocolCode: 0x02,
      },
    ]);
    expect(result.unprotected).toEqual([
      {
        configurationValue: 1,
        interfaceNumber: 1,
        alternateSetting: 0,
        classCode: 0xff,
        subclassCode: 0x00,
        protocolCode: 0x00,
      },
    ]);
  });

  it("considers all configurations (not just the current one)", () => {
    const device = mockDevice([
      {
        configurationValue: 1,
        interfaces: [
          {
            interfaceNumber: 0,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0x03,
                interfaceSubclass: 0x00,
                interfaceProtocol: 0x00,
              },
            ],
          },
        ],
      },
      {
        configurationValue: 2,
        interfaces: [
          {
            interfaceNumber: 3,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0xff,
                interfaceSubclass: 0x00,
                interfaceProtocol: 0x00,
              },
            ],
          },
        ],
      },
    ]);

    const result = classifyWebUsbDevice(device);
    expect(result.hasUnprotectedInterfaces).toBe(true);
    expect(result.protected.map((entry) => entry.configurationValue)).toEqual([1]);
    expect(result.unprotected.map((entry) => entry.configurationValue)).toEqual([2]);
  });

  it("treats audio/video interfaces as protected", () => {
    expect(isProtectedInterfaceClass(0x01)).toBe(true);
    expect(isProtectedInterfaceClass(0x0e)).toBe(true);
    expect(isProtectedInterfaceClass(0x10)).toBe(true);

    const device = mockDevice([
      {
        configurationValue: 1,
        interfaces: [
          {
            interfaceNumber: 0,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0x01,
                interfaceSubclass: 0x01,
                interfaceProtocol: 0x00,
              },
            ],
          },
          {
            interfaceNumber: 1,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0x0e,
                interfaceSubclass: 0x01,
                interfaceProtocol: 0x00,
              },
            ],
          },
          {
            interfaceNumber: 2,
            alternates: [
              {
                alternateSetting: 0,
                interfaceClass: 0x10,
                interfaceSubclass: 0x01,
                interfaceProtocol: 0x00,
              },
            ],
          },
        ],
      },
    ]);

    const result = classifyWebUsbDevice(device);
    expect(result.hasAnyInterfaces).toBe(true);
    expect(result.hasUnprotectedInterfaces).toBe(false);
    expect(result.unprotected).toEqual([]);
    expect(result.protected.map((entry) => entry.classCode)).toEqual([0x01, 0x0e, 0x10]);
  });

  it("describeUsbClassCode returns names for known classes and 0x?? for unknown", () => {
    expect(describeUsbClassCode(0x03)).toBe("HID");
    expect(describeUsbClassCode(0x42)).toBe("0x42");
  });
});

