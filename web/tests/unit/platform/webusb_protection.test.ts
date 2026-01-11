import { describe, expect, it } from "vitest";

import { classifyWebUsbDevice } from "../../../src/platform/webusb_protection";

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

describe("webusb_protection.classifyWebUsbDevice", () => {
  it("treats HID-only devices as not having any unprotected interfaces", () => {
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
    expect(result.hasUnprotectedInterfaces).toBe(false);
  });

  it("treats vendor-specific-only devices as having unprotected interfaces", () => {
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
    expect(result.hasUnprotectedInterfaces).toBe(true);
    expect(result.protected).toEqual([]);
    expect(result.unprotected).toHaveLength(1);
  });

  it("classifies composite HID + vendor-specific devices into protected/unprotected lists", () => {
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

  it("treats audio/video class examples as protected", () => {
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
        ],
      },
    ]);

    const result = classifyWebUsbDevice(device);
    expect(result.hasUnprotectedInterfaces).toBe(false);
    expect(result.unprotected).toEqual([]);
    expect(result.protected.map((entry) => entry.classCode)).toEqual([0x01, 0x0e]);
  });
});

