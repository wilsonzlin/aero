import { describe, expect, it, vi } from "vitest";

import { MmioBus } from "../bus/mmio.ts";
import { PciBus } from "../bus/pci.ts";
import { PortIoBus } from "../bus/portio.ts";
import type { IrqSink } from "../device_manager";
import { VirtioSndPciDevice, type VirtioSndPciBridgeLike } from "./virtio_snd";

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

function readCapFieldU32(cfg: ReturnType<typeof makeCfgIo>, dev: number, fn: number, capOff: number, off: number): number {
  return cfg.readU32(dev, fn, capOff + off) >>> 0;
}

function probeMmio64BarSize(cfg: ReturnType<typeof makeCfgIo>, dev: number, fn: number, barOff: number): bigint {
  cfg.writeU32(dev, fn, barOff, 0xffff_ffff);
  cfg.writeU32(dev, fn, barOff + 4, 0xffff_ffff);
  const maskLow = cfg.readU32(dev, fn, barOff) >>> 0;
  const maskHigh = cfg.readU32(dev, fn, barOff + 4) >>> 0;
  const mask = (BigInt(maskHigh) << 32n) | BigInt(maskLow & 0xffff_fff0);
  return (~mask + 1n) & 0xffff_ffff_ffff_ffffn;
}

describe("io/devices/virtio_snd PCI config", () => {
  it("exposes canonical virtio vendor-specific capabilities at 0x40/0x50/0x64/0x74", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const bridge: VirtioSndPciBridgeLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => false,
      set_audio_ring_buffer: () => {},
      set_host_sample_rate_hz: () => {},
      set_mic_ring_buffer: () => {},
      set_capture_sample_rate_hz: () => {},
      free: () => {},
    };
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const dev = new VirtioSndPciDevice({ bridge, irqSink });
    expect(dev.bdf).toEqual({ bus: 0, device: 11, function: 0 });

    // Register at the canonical BDF via the device-provided defaults.
    const addr = pciBus.registerDevice(dev);
    expect(addr).toEqual(dev.bdf);

    const cfg = makeCfgIo(portBus);

    // Vendor/device IDs: 1AF4:1059
    expect(cfg.readU32(11, 0, 0x00)).toBe(0x1059_1af4);

    // Subsystem vendor: 1AF4; subsystem ID: 0x0019
    expect(cfg.readU32(11, 0, 0x2c)).toBe(0x0019_1af4);

    // Revision ID.
    expect(cfg.readU8(11, 0, 0x08)).toBe(0x01);

    // Class code: 0x04_01_00 (Multimedia, Audio).
    expect(cfg.readU8(11, 0, 0x09)).toBe(0x00); // prog-if
    expect(cfg.readU8(11, 0, 0x0a)).toBe(0x01); // subclass
    expect(cfg.readU8(11, 0, 0x0b)).toBe(0x04); // base class

    // Interrupt pin should be INTA#.
    expect(cfg.readU8(11, 0, 0x3d)).toBe(0x01);

    // BAR0: 64-bit MMIO with size 0x4000.
    const bar0Low = cfg.readU32(11, 0, 0x10);
    const bar0High = cfg.readU32(11, 0, 0x14);
    expect(bar0Low & 0x0f).toBe(0x04);
    expect(bar0High).toBe(0x0000_0000);
    const size = probeMmio64BarSize(cfg, 11, 0, 0x10);
    expect(size).toBe(0x4000n);

    // Capability list present.
    const status = cfg.readU16(11, 0, 0x06);
    expect(status & 0x0010).toBe(0x0010);
    expect(cfg.readU8(11, 0, 0x34)).toBe(0x40);

    // Cap chain: 0x40 -> 0x50 -> 0x64 -> 0x74 -> 0x00
    expect(cfg.readU8(11, 0, 0x40)).toBe(0x09);
    expect(cfg.readU8(11, 0, 0x41)).toBe(0x50);
    expect(cfg.readU8(11, 0, 0x50)).toBe(0x09);
    expect(cfg.readU8(11, 0, 0x51)).toBe(0x64);
    expect(cfg.readU8(11, 0, 0x64)).toBe(0x09);
    expect(cfg.readU8(11, 0, 0x65)).toBe(0x74);
    expect(cfg.readU8(11, 0, 0x74)).toBe(0x09);
    expect(cfg.readU8(11, 0, 0x75)).toBe(0x00);

    // COMMON_CFG @ 0x40 (cap_len=16)
    expect(cfg.readU8(11, 0, 0x42)).toBe(16);
    expect(cfg.readU8(11, 0, 0x43)).toBe(1); // cfg_type
    expect(cfg.readU8(11, 0, 0x44)).toBe(0); // bar
    expect(readCapFieldU32(cfg, 11, 0, 0x40, 8)).toBe(0x0000);
    expect(readCapFieldU32(cfg, 11, 0, 0x40, 12)).toBe(0x0100);

    // NOTIFY_CFG @ 0x50 (cap_len=20, notify_off_multiplier=4)
    expect(cfg.readU8(11, 0, 0x52)).toBe(20);
    expect(cfg.readU8(11, 0, 0x53)).toBe(2);
    expect(cfg.readU8(11, 0, 0x54)).toBe(0);
    expect(readCapFieldU32(cfg, 11, 0, 0x50, 8)).toBe(0x1000);
    expect(readCapFieldU32(cfg, 11, 0, 0x50, 12)).toBe(0x0100);
    expect(readCapFieldU32(cfg, 11, 0, 0x50, 16)).toBe(4);

    // ISR_CFG @ 0x64 (cap_len=16)
    expect(cfg.readU8(11, 0, 0x66)).toBe(16);
    expect(cfg.readU8(11, 0, 0x67)).toBe(3);
    expect(cfg.readU8(11, 0, 0x68)).toBe(0);
    expect(readCapFieldU32(cfg, 11, 0, 0x64, 8)).toBe(0x2000);
    expect(readCapFieldU32(cfg, 11, 0, 0x64, 12)).toBe(0x0020);

    // DEVICE_CFG @ 0x74 (cap_len=16)
    expect(cfg.readU8(11, 0, 0x76)).toBe(16);
    expect(cfg.readU8(11, 0, 0x77)).toBe(4);
    expect(cfg.readU8(11, 0, 0x78)).toBe(0);
    expect(readCapFieldU32(cfg, 11, 0, 0x74, 8)).toBe(0x3000);
    expect(readCapFieldU32(cfg, 11, 0, 0x74, 12)).toBe(0x0100);
  });

  it("exposes BAR2 as IO for transitional/legacy modes and hides modern capabilities in legacy mode", () => {
    const make = (mode: "transitional" | "legacy") => {
      const portBus = new PortIoBus();
      const mmioBus = new MmioBus();
      const pciBus = new PciBus(portBus, mmioBus);
      pciBus.registerToPortBus();

      const bridge: VirtioSndPciBridgeLike = {
        mmio_read: () => 0,
        mmio_write: () => {},
        poll: () => {},
        driver_ok: () => false,
        irq_asserted: () => false,
        set_audio_ring_buffer: () => {},
        set_host_sample_rate_hz: () => {},
        set_mic_ring_buffer: () => {},
        set_capture_sample_rate_hz: () => {},
        free: () => {},
      };
      const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
      const dev = new VirtioSndPciDevice({ bridge, irqSink, mode });
      pciBus.registerDevice(dev);
      return { cfg: makeCfgIo(portBus) };
    };

    // Transitional: device ID should be the 0x1000-range transitional ID and modern capabilities should be present.
    {
      const { cfg } = make("transitional");
      expect(cfg.readU32(11, 0, 0x00)).toBe(0x1018_1af4);

      const bar2 = cfg.readU32(11, 0, 0x18);
      expect(bar2 & 0x1).toBe(0x1);
      // Probe BAR2 size mask.
      cfg.writeU32(11, 0, 0x18, 0xffff_ffff);
      expect(cfg.readU32(11, 0, 0x18)).toBe(0xffff_ff01);

      const status = cfg.readU16(11, 0, 0x06);
      expect(status & 0x0010).toBe(0x0010);
      expect(cfg.readU8(11, 0, 0x34)).toBe(0x40);
    }

    // Legacy-only: same 0x1000-range device ID, BAR2 present, but modern capability list disabled.
    {
      const { cfg } = make("legacy");
      expect(cfg.readU32(11, 0, 0x00)).toBe(0x1018_1af4);

      const bar2 = cfg.readU32(11, 0, 0x18);
      expect(bar2 & 0x1).toBe(0x1);
      cfg.writeU32(11, 0, 0x18, 0xffff_ffff);
      expect(cfg.readU32(11, 0, 0x18)).toBe(0xffff_ff01);

      const status = cfg.readU16(11, 0, 0x06);
      expect(status & 0x0010).toBe(0x0000);
      expect(cfg.readU8(11, 0, 0x34)).toBe(0x00);
    }
  });
});

describe("io/devices/virtio_snd BAR0 MMIO", () => {
  it("accepts camelCase virtio-snd bridge exports (backwards compatibility)", () => {
    const mmioRead = vi.fn(() => 0x1234_5678);
    const mmioWrite = vi.fn();
    const poll = vi.fn();
    const driverOk = vi.fn(() => false);
    const irqAsserted = vi.fn(() => false);
    const setAudioRingBuffer = vi.fn();
    const setHostSampleRateHz = vi.fn();
    const setMicRingBuffer = vi.fn();
    const setCaptureSampleRateHz = vi.fn();
    const setPciCommand = vi.fn();
    const free = vi.fn();

    const bridge = {
      mmioRead,
      mmioWrite,
      poll,
      driverOk,
      irqAsserted,
      setAudioRingBuffer,
      setHostSampleRateHz,
      setMicRingBuffer,
      setCaptureSampleRateHz,
      setPciCommand,
      free,
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioSndPciDevice({ bridge: bridge as unknown as VirtioSndPciBridgeLike, irqSink });

    // Defined BAR0 region should forward to bridge.
    expect(dev.mmioRead(0, 0x0000n, 4)).toBe(0x1234_5678);
    expect(mmioRead).toHaveBeenCalledWith(0, 4);
    dev.mmioWrite(0, 0x0000n, 4, 0xdead_beef);
    expect(mmioWrite).toHaveBeenCalledWith(0, 4, 0xdead_beef);

    // Output sample rate + audio ring plumbing.
    dev.setAudioRingBuffer({ ringBuffer: null, capacityFrames: 0, channelCount: 0, dstSampleRateHz: 48_000 });
    expect(setHostSampleRateHz).toHaveBeenCalledWith(48_000);
    expect(setAudioRingBuffer).toHaveBeenCalledWith(undefined, 0, 0);

    dev.setCaptureSampleRateHz(44_100);
    expect(setCaptureSampleRateHz).toHaveBeenCalledWith(44_100);

    const ringBuffer =
      typeof SharedArrayBuffer === "function" ? new SharedArrayBuffer(256) : ({} as unknown as SharedArrayBuffer);
    dev.setMicRingBuffer(ringBuffer);
    expect(setMicRingBuffer).toHaveBeenCalledWith(ringBuffer);

    // Polling is gated on bus mastering.
    dev.tick(0);
    expect(poll).not.toHaveBeenCalled();
    dev.onPciCommandWrite(1 << 2);
    expect(setPciCommand).toHaveBeenCalledWith(0x0004);
    dev.tick(1);
    expect(poll).toHaveBeenCalledTimes(1);

    dev.destroy();
    expect(free).toHaveBeenCalled();
  });

  it("returns 0 and ignores writes for undefined BAR0 MMIO offsets (contract v1)", () => {
    const mmioRead = vi.fn(() => 0x1234_5678);
    const mmioWrite = vi.fn();
    const bridge: VirtioSndPciBridgeLike = {
      mmio_read: mmioRead,
      mmio_write: mmioWrite,
      poll: vi.fn(),
      driver_ok: vi.fn(() => false),
      irq_asserted: vi.fn(() => false),
      set_audio_ring_buffer: vi.fn(),
      set_host_sample_rate_hz: vi.fn(),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      free: vi.fn(),
    };

    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioSndPciDevice({ bridge, irqSink });

    // Defined region (COMMON_CFG): should forward to bridge.
    expect(dev.mmioRead(0, 0x0000n, 4)).toBe(0x1234_5678);
    expect(mmioRead).toHaveBeenCalledWith(0, 4);

    mmioRead.mockClear();
    // Undefined region within BAR0: must read as 0 and must not hit the bridge.
    expect(dev.mmioRead(0, 0x0400n, 4)).toBe(0);
    expect(mmioRead).not.toHaveBeenCalled();

    // Crossing a defined region boundary counts as undefined for the requested width.
    expect(dev.mmioRead(0, 0x00ffn, 4)).toBe(0);

    // Undefined writes are ignored (no bridge call).
    dev.mmioWrite(0, 0x0400n, 4, 0xdead_beef);
    expect(mmioWrite).not.toHaveBeenCalled();

    // Defined writes are forwarded.
    dev.mmioWrite(0, 0x0000n, 4, 0xdead_beef);
    expect(mmioWrite).toHaveBeenCalledWith(0, 4, 0xdead_beef);
  });
});

describe("io/devices/virtio_snd PCI command semantics", () => {
  it("gates device polling on PCI Bus Master Enable (command bit 2)", () => {
    const poll = vi.fn();
    const bridge: VirtioSndPciBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      poll,
      driver_ok: vi.fn(() => false),
      irq_asserted: vi.fn(() => false),
      set_audio_ring_buffer: vi.fn(),
      set_host_sample_rate_hz: vi.fn(),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioSndPciDevice({ bridge, irqSink });

    // Not bus-master enabled by default; tick should not poll the device.
    dev.tick(0);
    expect(poll).not.toHaveBeenCalled();

    // Enable BME (bit 2).
    dev.onPciCommandWrite(1 << 2);
    dev.tick(1);
    expect(poll).toHaveBeenCalledTimes(1);
  });

  it("respects PCI command Interrupt Disable bit (bit 10) when syncing INTx level", () => {
    let irq = false;
    const bridge: VirtioSndPciBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      poll: vi.fn(),
      driver_ok: vi.fn(() => false),
      irq_asserted: vi.fn(() => irq),
      set_audio_ring_buffer: vi.fn(),
      set_host_sample_rate_hz: vi.fn(),
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: vi.fn(),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioSndPciDevice({ bridge, irqSink });

    // Start deasserted.
    dev.tick(0);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    // Assert line.
    irq = true;
    dev.tick(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(9);

    // Disable INTx in PCI command register: should drop the line.
    dev.onPciCommandWrite(1 << 10);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledWith(9);

    // Re-enable INTx: should reassert because the device-level condition is still true.
    dev.onPciCommandWrite(0);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(2);
    expect(irqSink.raiseIrq).toHaveBeenLastCalledWith(9);
  });
});

describe("io/devices/virtio_snd audio ring attachment", () => {
  it("reasserts capture sample rate after updating the host/output sample rate", () => {
    const setHostRate = vi.fn();
    const setCaptureRate = vi.fn();
    const setAudioRing = vi.fn();

    const bridge: VirtioSndPciBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      poll: vi.fn(),
      driver_ok: vi.fn(() => false),
      irq_asserted: vi.fn(() => false),
      set_audio_ring_buffer: setAudioRing,
      set_host_sample_rate_hz: setHostRate,
      set_mic_ring_buffer: vi.fn(),
      set_capture_sample_rate_hz: setCaptureRate,
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioSndPciDevice({ bridge, irqSink });

    const ringBuffer =
      typeof SharedArrayBuffer === "function" ? new SharedArrayBuffer(256) : ({} as unknown as SharedArrayBuffer);

    // Configure a mic capture rate that differs from the output rate, then update the output rate
    // via setAudioRingBuffer. The wrapper should reassert the capture rate so virtio-snd capture
    // does not drift back to tracking output.
    dev.setCaptureSampleRateHz(44_100);
    setCaptureRate.mockClear();

    dev.setAudioRingBuffer({ ringBuffer, capacityFrames: 128, channelCount: 2, dstSampleRateHz: 48_000 });
    expect(setHostRate).toHaveBeenCalledWith(48_000);
    expect(setAudioRing).toHaveBeenCalledWith(ringBuffer, 128, 2);
    expect(setCaptureRate).toHaveBeenCalledWith(44_100);
  });
});
