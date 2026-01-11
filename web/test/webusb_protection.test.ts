import test from "node:test";
import assert from "node:assert/strict";

import { classifyWebUsbDevice } from "../src/platform/webusb_protection.ts";

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

test("classifyWebUsbDevice: HID-only device has no unprotected interfaces", () => {
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
  assert.equal(result.hasAnyInterfaces, true);
  assert.equal(result.hasUnprotectedInterfaces, false);
  assert.deepEqual(result.unprotected, []);
  assert.deepEqual(result.protected, [
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

test("classifyWebUsbDevice: vendor-specific-only device is requestable", () => {
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
  assert.equal(result.hasAnyInterfaces, true);
  assert.equal(result.hasUnprotectedInterfaces, true);
  assert.deepEqual(result.protected, []);
  assert.deepEqual(result.unprotected, [
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

test("classifyWebUsbDevice: composite HID + vendor-specific device is requestable", () => {
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
  assert.equal(result.hasAnyInterfaces, true);
  assert.equal(result.hasUnprotectedInterfaces, true);
  assert.deepEqual(result.protected, [
    {
      configurationValue: 1,
      interfaceNumber: 0,
      alternateSetting: 0,
      classCode: 0x03,
      subclassCode: 0x01,
      protocolCode: 0x02,
    },
  ]);
  assert.deepEqual(result.unprotected, [
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

test("classifyWebUsbDevice: audio/video interfaces are treated as protected", () => {
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
  assert.equal(result.hasAnyInterfaces, true);
  assert.equal(result.hasUnprotectedInterfaces, false);
  assert.deepEqual(result.unprotected, []);
  assert.equal(result.protected.length, 3);
  assert.deepEqual(
    result.protected.map((entry) => entry.classCode),
    [0x01, 0x0e, 0x10],
  );
});

