import { describe, expect, it } from "vitest";

import { MmioBus } from "../bus/mmio.ts";
import { PciBus } from "../bus/pci.ts";
import { PortIoBus } from "../bus/portio.ts";
import { VirtioInputPciFunction, hidUsageToLinuxKeyCode, type VirtioInputPciDeviceLike } from "./virtio_input.ts";

function cfgAddr(dev: number, fn: number, off: number): number {
  // PCI config mechanism #1 (I/O ports 0xCF8/0xCFC).
  return (0x8000_0000 | ((dev & 0x1f) << 11) | ((fn & 0x07) << 8) | (off & 0xfc)) >>> 0;
}

function makeCfgIo(portBus: PortIoBus) {
  return {
    readU32(dev: number, fn: number, off: number): number {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      return portBus.read(0x0cfc, 4) >>> 0;
    },
    readU16(dev: number, fn: number, off: number): number {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      return portBus.read(0x0cfc + (off & 3), 2) & 0xffff;
    },
    readU8(dev: number, fn: number, off: number): number {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      return portBus.read(0x0cfc + (off & 3), 1) & 0xff;
    },
    writeU32(dev: number, fn: number, off: number, value: number): void {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      portBus.write(0x0cfc, 4, value >>> 0);
    },
  };
}

const dummyDevice: VirtioInputPciDeviceLike = {
  mmio_read: (_offset, _size) => 0,
  mmio_write: (_offset, _size, _value) => {},
  poll: () => {},
  driver_ok: () => false,
  irq_asserted: () => false,
  inject_key: (_linuxKey, _pressed) => {},
  inject_rel: (_dx, _dy) => {},
  inject_button: (_btn, _pressed) => {},
  inject_wheel: (_delta) => {},
  free: () => {},
};

describe("io/devices/virtio_input", () => {
  it("exposes contract PCI IDs + virtio vendor capabilities (keyboard+mouse functions)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const irqSink = { raiseIrq: (_irq: number) => {}, lowerIrq: (_irq: number) => {} };

    const keyboard = new VirtioInputPciFunction({ kind: "keyboard", device: dummyDevice, irqSink });
    const mouse = new VirtioInputPciFunction({ kind: "mouse", device: dummyDevice, irqSink });

    pciBus.registerDevice(keyboard, { device: 0, function: 0 });
    pciBus.registerDevice(mouse, { device: 0, function: 1 });

    const cfg = makeCfgIo(portBus);

    // Vendor / device IDs.
    expect(cfg.readU32(0, 0, 0x00)).toBe(0x1052_1af4);
    expect(cfg.readU32(0, 1, 0x00)).toBe(0x1052_1af4);

    // Revision + class code.
    expect(cfg.readU8(0, 0, 0x08)).toBe(0x01);
    expect(cfg.readU8(0, 0, 0x09)).toBe(0x00); // prog-if
    expect(cfg.readU8(0, 0, 0x0a)).toBe(0x80); // subclass
    expect(cfg.readU8(0, 0, 0x0b)).toBe(0x09); // base class

    // Multi-function headerType bit must be set on function 0 only.
    expect(cfg.readU8(0, 0, 0x0e)).toBe(0x80);
    expect(cfg.readU8(0, 1, 0x0e)).toBe(0x00);

    // Subsystem IDs distinguish keyboard vs mouse.
    expect(cfg.readU32(0, 0, 0x2c)).toBe(0x0010_1af4);
    expect(cfg.readU32(0, 1, 0x2c)).toBe(0x0011_1af4);

    // Interrupt line/pin.
    expect(cfg.readU8(0, 0, 0x3c)).toBe(0x05);
    expect(cfg.readU8(0, 0, 0x3d)).toBe(0x01); // INTA#

    // BAR0 must be 64-bit MMIO.
    expect(cfg.readU32(0, 0, 0x10) & 0xf).toBe(0x4);
    expect(cfg.readU32(0, 0, 0x14)).toBe(0x0000_0000);

    // Cap list must be present and start at 0x50 per Aero virtio contract v1.
    expect(cfg.readU16(0, 0, 0x06) & 0x0010).toBe(0x0010);
    expect(cfg.readU8(0, 0, 0x34)).toBe(0x50);

    // COMMON cap @0x50.
    expect(cfg.readU8(0, 0, 0x50)).toBe(0x09);
    expect(cfg.readU8(0, 0, 0x51)).toBe(0x60);
    expect(cfg.readU8(0, 0, 0x52)).toBe(16);
    expect(cfg.readU8(0, 0, 0x53)).toBe(1);
    expect(cfg.readU8(0, 0, 0x54)).toBe(0);
    expect(cfg.readU32(0, 0, 0x58)).toBe(0x0000);
    expect(cfg.readU32(0, 0, 0x5c)).toBe(0x0100);

    // NOTIFY cap @0x60.
    expect(cfg.readU8(0, 0, 0x60)).toBe(0x09);
    expect(cfg.readU8(0, 0, 0x61)).toBe(0x74);
    expect(cfg.readU8(0, 0, 0x62)).toBe(20);
    expect(cfg.readU8(0, 0, 0x63)).toBe(2);
    expect(cfg.readU8(0, 0, 0x64)).toBe(0);
    expect(cfg.readU32(0, 0, 0x68)).toBe(0x1000);
    expect(cfg.readU32(0, 0, 0x6c)).toBe(0x0100);
    expect(cfg.readU32(0, 0, 0x70)).toBe(4);

    // ISR cap @0x74.
    expect(cfg.readU8(0, 0, 0x74)).toBe(0x09);
    expect(cfg.readU8(0, 0, 0x75)).toBe(0x84);
    expect(cfg.readU8(0, 0, 0x76)).toBe(16);
    expect(cfg.readU8(0, 0, 0x77)).toBe(3);
    expect(cfg.readU8(0, 0, 0x78)).toBe(0);
    expect(cfg.readU32(0, 0, 0x7c)).toBe(0x2000);
    expect(cfg.readU32(0, 0, 0x80)).toBe(0x0020);

    // DEVICE cap @0x84.
    expect(cfg.readU8(0, 0, 0x84)).toBe(0x09);
    expect(cfg.readU8(0, 0, 0x85)).toBe(0x00);
    expect(cfg.readU8(0, 0, 0x86)).toBe(16);
    expect(cfg.readU8(0, 0, 0x87)).toBe(4);
    expect(cfg.readU8(0, 0, 0x88)).toBe(0);
    expect(cfg.readU32(0, 0, 0x8c)).toBe(0x3000);
    expect(cfg.readU32(0, 0, 0x90)).toBe(0x0100);

    // BAR sizing probe should reflect 0x4000.
    cfg.writeU32(0, 0, 0x10, 0xffff_ffff);
    cfg.writeU32(0, 0, 0x14, 0xffff_ffff);
    expect(cfg.readU32(0, 0, 0x10)).toBe(0xffff_c004);
    expect(cfg.readU32(0, 0, 0x14)).toBe(0xffff_ffff);
  });
});

describe("hidUsageToLinuxKeyCode", () => {
  it("maps modifier (GUI) keys and lock keys used by the virtio-input path", () => {
    // Modifiers (HID usages 0xE0..=0xE7).
    expect(hidUsageToLinuxKeyCode(0xe3)).toBe(125); // KEY_LEFTMETA
    expect(hidUsageToLinuxKeyCode(0xe7)).toBe(126); // KEY_RIGHTMETA

    // Locks / system.
    expect(hidUsageToLinuxKeyCode(0x47)).toBe(70); // KEY_SCROLLLOCK
    expect(hidUsageToLinuxKeyCode(0x53)).toBe(69); // KEY_NUMLOCK
  });

  it("maps contract-required alphanumerics and basic keys", () => {
    // A..Z.
    expect(hidUsageToLinuxKeyCode(0x04)).toBe(30);
    expect(hidUsageToLinuxKeyCode(0x1d)).toBe(44);

    // 0..9.
    expect(hidUsageToLinuxKeyCode(0x27)).toBe(11);

    // Enter / Esc.
    expect(hidUsageToLinuxKeyCode(0x28)).toBe(28);
    expect(hidUsageToLinuxKeyCode(0x29)).toBe(1);
  });

  it("maps contract-required function keys", () => {
    expect(hidUsageToLinuxKeyCode(0x3a)).toBe(59); // KEY_F1
    expect(hidUsageToLinuxKeyCode(0x45)).toBe(88); // KEY_F12
  });

  it("maps common punctuation keys used for text entry", () => {
    expect(hidUsageToLinuxKeyCode(0x2d)).toBe(12); // KEY_MINUS
    expect(hidUsageToLinuxKeyCode(0x2e)).toBe(13); // KEY_EQUAL
    expect(hidUsageToLinuxKeyCode(0x2f)).toBe(26); // KEY_LEFTBRACE
    expect(hidUsageToLinuxKeyCode(0x30)).toBe(27); // KEY_RIGHTBRACE
    expect(hidUsageToLinuxKeyCode(0x31)).toBe(43); // KEY_BACKSLASH
    expect(hidUsageToLinuxKeyCode(0x33)).toBe(39); // KEY_SEMICOLON
    expect(hidUsageToLinuxKeyCode(0x34)).toBe(40); // KEY_APOSTROPHE
    expect(hidUsageToLinuxKeyCode(0x35)).toBe(41); // KEY_GRAVE
    expect(hidUsageToLinuxKeyCode(0x36)).toBe(51); // KEY_COMMA
    expect(hidUsageToLinuxKeyCode(0x37)).toBe(52); // KEY_DOT
    expect(hidUsageToLinuxKeyCode(0x38)).toBe(53); // KEY_SLASH
  });
});
