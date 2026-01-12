import { describe, expect, it } from "vitest";

import { computeGuestRamLayout, guestToLinear } from "../../runtime/shared_layout";
import { assertWasmMemoryWiring } from "../../runtime/wasm_memory_probe";
import { initWasm } from "../../runtime/wasm_loader";
import { DeviceManager, type IrqSink } from "../device_manager";
import type { PciAddress } from "../bus/pci";
import { VirtioInputPciFunction } from "./virtio_input";

// Virtio status flags (virtio spec).
const VIRTIO_STATUS_ACKNOWLEDGE = 1;
const VIRTIO_STATUS_DRIVER = 2;
const VIRTIO_STATUS_DRIVER_OK = 4;
const VIRTIO_STATUS_FEATURES_OK = 8;

// Virtqueue descriptor flags.
const VIRTQ_DESC_F_WRITE = 2;

// Linux input ABI (matches `crates/aero-virtio/src/devices/input.rs`).
const EV_SYN = 0;
const EV_KEY = 1;
const EV_REL = 2;
const SYN_REPORT = 0;
const KEY_A = 30;
const REL_X = 0;
const REL_Y = 1;
const REL_WHEEL = 8;
const BTN_LEFT = 0x110;

function cfgAddr(addr: PciAddress, off: number): number {
  // PCI config mechanism #1 (I/O ports 0xCF8/0xCFC).
  return (0x8000_0000 | ((addr.bus & 0xff) << 16) | ((addr.device & 0x1f) << 11) | ((addr.function & 0x07) << 8) | (off & 0xfc)) >>> 0;
}

describe("io/devices/virtio-input (pci bridge integration)", () => {
  it("injects EV_KEY/EV_SYN events via the virtio-pci BAR0 MMIO interface", async () => {
    const desiredGuestBytes = 0x20_000; // 128 KiB
    const layout = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layout.wasm_pages, maximum: layout.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // The wasm-pack output is generated and may be absent in some test environments.
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "virtio_input_pci_integration.test" });

    const Ctor = api.VirtioInputPciDevice;
    if (!Ctor) return;

    const irqState = { raised: 0, lowered: 0, asserted: false };
    const irqSink: IrqSink = {
      raiseIrq: (_irq: number) => {
        irqState.raised += 1;
        irqState.asserted = true;
      },
      lowerIrq: (_irq: number) => {
        irqState.lowered += 1;
        irqState.asserted = false;
      },
    };

    const mgr = new DeviceManager(irqSink);

    const bridge = new Ctor(layout.guest_base >>> 0, layout.guest_size >>> 0, "keyboard");
    const dev = new VirtioInputPciFunction({ kind: "keyboard", device: bridge, irqSink: mgr.irqSink });

    const dv = new DataView(memory.buffer);

    const guestWriteU16 = (paddr: number, value: number) => dv.setUint16(guestToLinear(layout, paddr), value & 0xffff, true);
    const guestWriteU32 = (paddr: number, value: number) => dv.setUint32(guestToLinear(layout, paddr), value >>> 0, true);
    const guestWriteBytes = (paddr: number, bytes: Uint8Array) => {
      new Uint8Array(memory.buffer, guestToLinear(layout, paddr), bytes.byteLength).set(bytes);
    };
    const guestReadU16 = (paddr: number) => dv.getUint16(guestToLinear(layout, paddr), true) >>> 0;
    const guestReadU32 = (paddr: number) => dv.getUint32(guestToLinear(layout, paddr), true) >>> 0;
    const guestReadBytes = (paddr: number, len: number): Uint8Array => {
      return new Uint8Array(memory.buffer, guestToLinear(layout, paddr), len).slice();
    };
    const guestWriteDesc = (table: number, index: number, addr: number, len: number, flags: number, next: number) => {
      const base = table + index * 16;
      // u64 addr (low, then high=0)
      dv.setUint32(guestToLinear(layout, base), addr >>> 0, true);
      dv.setUint32(guestToLinear(layout, base + 4), 0, true);
      dv.setUint32(guestToLinear(layout, base + 8), len >>> 0, true);
      dv.setUint16(guestToLinear(layout, base + 12), flags & 0xffff, true);
      dv.setUint16(guestToLinear(layout, base + 14), next & 0xffff, true);
    };

    const cfgReadU16 = (addr: PciAddress, off: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      return mgr.portRead(0x0cfc + (off & 3), 2) & 0xffff;
    };
    const cfgReadU32 = (addr: PciAddress, off: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      return mgr.portRead(0x0cfc + (off & 3), 4) >>> 0;
    };
    const cfgWriteU16 = (addr: PciAddress, off: number, value: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      mgr.portWrite(0x0cfc + (off & 3), 2, value & 0xffff);
    };
    const cfgWriteU32 = (addr: PciAddress, off: number, value: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      mgr.portWrite(0x0cfc + (off & 3), 4, value >>> 0);
    };

    const mmioReadU8 = (addr: bigint) => mgr.mmioRead(addr, 1) & 0xff;
    const mmioReadU16 = (addr: bigint) => mgr.mmioRead(addr, 2) & 0xffff;
    const mmioReadU32 = (addr: bigint) => mgr.mmioRead(addr, 4) >>> 0;
    const mmioWriteU8 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 1, value & 0xff);
    const mmioWriteU16 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 2, value & 0xffff);
    const mmioWriteU32 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 4, value >>> 0);
    const mmioWriteU64 = (addr: bigint, value: bigint) => {
      mmioWriteU32(addr, Number(value & 0xffff_ffffn));
      mmioWriteU32(addr + 4n, Number((value >> 32n) & 0xffff_ffffn));
    };

    let pciAddr: PciAddress | null = null;
    try {
      pciAddr = mgr.registerPciDevice(dev);
      mgr.addTickable(dev);

      // Move BAR0 above 4GiB to exercise mmio64 base+high dword plumbing.
      const bar0LowInitial = cfgReadU32(pciAddr, 0x10);
      const barAttrBits = bar0LowInitial & 0x0f;
      const newBarBase = 0x1_0000_0000n; // 4GiB
      const newBar0Low = ((Number(newBarBase & 0xffff_ffffn) & 0xffff_fff0) | barAttrBits) >>> 0;
      const newBar0High = Number((newBarBase >> 32n) & 0xffff_ffffn) >>> 0;
      cfgWriteU32(pciAddr, 0x10, newBar0Low);
      cfgWriteU32(pciAddr, 0x14, newBar0High);

      // Enable PCI memory decoding.
      cfgWriteU16(pciAddr, 0x04, cfgReadU16(pciAddr, 0x04) | 0x2);

      const bar0Low = cfgReadU32(pciAddr, 0x10);
      const bar0High = cfgReadU32(pciAddr, 0x14);
      const bar0Base = (BigInt(bar0High) << 32n) | (BigInt(bar0Low) & 0xffff_fff0n);
      expect(bar0Base).toBe(newBarBase);

      // Contract v1 fixed BAR0 layout.
      const commonBase = bar0Base + 0x0000n;
      const notifyBase = bar0Base + 0x1000n;
      const isrBase = bar0Base + 0x2000n;

      // -------------------------------------------------------------------------------------------
      // Virtio modern init (feature negotiation).
      // -------------------------------------------------------------------------------------------
      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE);
      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

      for (const sel of [0, 1]) {
        mmioWriteU32(commonBase + 0x00n, sel);
        const f = mmioReadU32(commonBase + 0x04n);
        mmioWriteU32(commonBase + 0x08n, sel);
        mmioWriteU32(commonBase + 0x0cn, f);
      }

      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);
      mmioWriteU8(
        commonBase + 0x14n,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK,
      );
      expect(dev.driverOk()).toBe(true);

      // -------------------------------------------------------------------------------------------
      // Queue config (eventq = queue 0).
      // -------------------------------------------------------------------------------------------
      const desc = 0x1000;
      const avail = 0x2000;
      const used = 0x3000;
      const eventBufBase = 0x4000;

      mmioWriteU16(commonBase + 0x16n, 0);
      expect(mmioReadU16(commonBase + 0x18n)).toBe(64);
      expect(mmioReadU16(commonBase + 0x1en)).toBe(0);

      mmioWriteU64(commonBase + 0x20n, BigInt(desc));
      mmioWriteU64(commonBase + 0x28n, BigInt(avail));
      mmioWriteU64(commonBase + 0x30n, BigInt(used));
      mmioWriteU16(commonBase + 0x1cn, 1);

      const bufferCount = 4;
      for (let i = 0; i < bufferCount; i += 1) {
        const bufAddr = eventBufBase + i * 8;
        guestWriteBytes(bufAddr, new Uint8Array(8).fill(0xaa));
        guestWriteDesc(desc, i, bufAddr, 8, VIRTQ_DESC_F_WRITE, 0);
      }

      // Avail ring: flags=0, idx=bufferCount, ring[i]=descriptor index.
      guestWriteU16(avail + 0, 0);
      guestWriteU16(avail + 2, bufferCount);
      for (let i = 0; i < bufferCount; i += 1) {
        guestWriteU16(avail + 4 + i * 2, i);
      }
      // Used ring: flags=0, idx=0.
      guestWriteU16(used + 0, 0);
      guestWriteU16(used + 2, 0);
      for (let i = 0; i < bufferCount; i += 1) {
        guestWriteU32(used + 4 + i * 8 + 0, 0);
        guestWriteU32(used + 4 + i * 8 + 4, 0);
      }

      // Notify queue 0 (notify_off_multiplier is fixed to 4 in contract v1).
      mmioWriteU16(notifyBase + 0n, 0);
      expect(guestReadU16(used + 2)).toBe(0);

      // -------------------------------------------------------------------------------------------
      // Host injects a key press and we should observe EV_KEY + EV_SYN events.
      // -------------------------------------------------------------------------------------------
      dev.injectKey(KEY_A, true);
      expect(guestReadU16(used + 2)).toBe(2);

      const used0Id = guestReadU32(used + 4 + 0);
      const used0Len = guestReadU32(used + 4 + 4);
      const used1Id = guestReadU32(used + 4 + 8);
      const used1Len = guestReadU32(used + 4 + 12);
      expect(used0Id).toBe(0);
      expect(used0Len).toBe(8);
      expect(used1Id).toBe(1);
      expect(used1Len).toBe(8);

      const ev0 = guestReadBytes(eventBufBase + 0 * 8, 8);
      const ev1 = guestReadBytes(eventBufBase + 1 * 8, 8);
      expect(Array.from(ev0)).toEqual([
        EV_KEY & 0xff,
        (EV_KEY >>> 8) & 0xff,
        KEY_A & 0xff,
        (KEY_A >>> 8) & 0xff,
        1,
        0,
        0,
        0,
      ]);
      expect(Array.from(ev1)).toEqual([EV_SYN, 0, SYN_REPORT, 0, 0, 0, 0, 0]);

      // Queue interrupt should assert INTx.
      expect(irqState.raised).toBeGreaterThan(0);
      expect(irqState.asserted).toBe(true);

      // Reading ISR must acknowledge and deassert INTx.
      const isr = mmioReadU8(isrBase);
      expect(isr & 0x01).toBe(0x01);
      expect(irqState.lowered).toBeGreaterThan(0);
      expect(irqState.asserted).toBe(false);
    } finally {
      try {
        dev.destroy();
      } catch {
        // ignore
      }
      void pciAddr;
    }
  });

  it("injects EV_REL/EV_KEY mouse events via the virtio-pci BAR0 MMIO interface", async () => {
    const desiredGuestBytes = 0x20_000; // 128 KiB
    const layout = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layout.wasm_pages, maximum: layout.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // The wasm-pack output is generated and may be absent in some test environments.
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "virtio_input_pci_integration.test (mouse)" });

    const Ctor = api.VirtioInputPciDevice;
    if (!Ctor) return;

    const irqState = { raised: 0, lowered: 0, asserted: false };
    const irqSink: IrqSink = {
      raiseIrq: (_irq: number) => {
        irqState.raised += 1;
        irqState.asserted = true;
      },
      lowerIrq: (_irq: number) => {
        irqState.lowered += 1;
        irqState.asserted = false;
      },
    };

    const mgr = new DeviceManager(irqSink);

    const bridge = new Ctor(layout.guest_base >>> 0, layout.guest_size >>> 0, "mouse");
    const dev = new VirtioInputPciFunction({ kind: "mouse", device: bridge, irqSink: mgr.irqSink });

    const dv = new DataView(memory.buffer);

    const guestWriteU16 = (paddr: number, value: number) => dv.setUint16(guestToLinear(layout, paddr), value & 0xffff, true);
    const guestWriteU32 = (paddr: number, value: number) => dv.setUint32(guestToLinear(layout, paddr), value >>> 0, true);
    const guestWriteBytes = (paddr: number, bytes: Uint8Array) => {
      new Uint8Array(memory.buffer, guestToLinear(layout, paddr), bytes.byteLength).set(bytes);
    };
    const guestReadU16 = (paddr: number) => dv.getUint16(guestToLinear(layout, paddr), true) >>> 0;
    const guestReadU32 = (paddr: number) => dv.getUint32(guestToLinear(layout, paddr), true) >>> 0;
    const guestReadBytes = (paddr: number, len: number): Uint8Array => {
      return new Uint8Array(memory.buffer, guestToLinear(layout, paddr), len).slice();
    };
    const guestWriteDesc = (table: number, index: number, addr: number, len: number, flags: number, next: number) => {
      const base = table + index * 16;
      // u64 addr (low, then high=0)
      dv.setUint32(guestToLinear(layout, base), addr >>> 0, true);
      dv.setUint32(guestToLinear(layout, base + 4), 0, true);
      dv.setUint32(guestToLinear(layout, base + 8), len >>> 0, true);
      dv.setUint16(guestToLinear(layout, base + 12), flags & 0xffff, true);
      dv.setUint16(guestToLinear(layout, base + 14), next & 0xffff, true);
    };

    const cfgReadU16 = (addr: PciAddress, off: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      return mgr.portRead(0x0cfc + (off & 3), 2) & 0xffff;
    };
    const cfgReadU32 = (addr: PciAddress, off: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      return mgr.portRead(0x0cfc + (off & 3), 4) >>> 0;
    };
    const cfgWriteU16 = (addr: PciAddress, off: number, value: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      mgr.portWrite(0x0cfc + (off & 3), 2, value & 0xffff);
    };
    const cfgWriteU32 = (addr: PciAddress, off: number, value: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      mgr.portWrite(0x0cfc + (off & 3), 4, value >>> 0);
    };

    const mmioReadU8 = (addr: bigint) => mgr.mmioRead(addr, 1) & 0xff;
    const mmioReadU16 = (addr: bigint) => mgr.mmioRead(addr, 2) & 0xffff;
    const mmioReadU32 = (addr: bigint) => mgr.mmioRead(addr, 4) >>> 0;
    const mmioWriteU8 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 1, value & 0xff);
    const mmioWriteU16 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 2, value & 0xffff);
    const mmioWriteU32 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 4, value >>> 0);
    const mmioWriteU64 = (addr: bigint, value: bigint) => {
      mmioWriteU32(addr, Number(value & 0xffff_ffffn));
      mmioWriteU32(addr + 4n, Number((value >> 32n) & 0xffff_ffffn));
    };

    const decodeEvent = (bytes: Uint8Array): { type_: number; code: number; value: number } => {
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      return {
        type_: view.getUint16(0, true) >>> 0,
        code: view.getUint16(2, true) >>> 0,
        value: view.getInt32(4, true) | 0,
      };
    };

    let pciAddr: PciAddress | null = null;
    try {
      pciAddr = mgr.registerPciDevice(dev);
      mgr.addTickable(dev);

      // Move BAR0 above 4GiB to exercise mmio64 base+high dword plumbing.
      const bar0LowInitial = cfgReadU32(pciAddr, 0x10);
      const barAttrBits = bar0LowInitial & 0x0f;
      const newBarBase = 0x1_0000_0000n; // 4GiB
      const newBar0Low = ((Number(newBarBase & 0xffff_ffffn) & 0xffff_fff0) | barAttrBits) >>> 0;
      const newBar0High = Number((newBarBase >> 32n) & 0xffff_ffffn) >>> 0;
      cfgWriteU32(pciAddr, 0x10, newBar0Low);
      cfgWriteU32(pciAddr, 0x14, newBar0High);

      // Enable PCI memory decoding.
      cfgWriteU16(pciAddr, 0x04, cfgReadU16(pciAddr, 0x04) | 0x2);

      const bar0Low = cfgReadU32(pciAddr, 0x10);
      const bar0High = cfgReadU32(pciAddr, 0x14);
      const bar0Base = (BigInt(bar0High) << 32n) | (BigInt(bar0Low) & 0xffff_fff0n);
      expect(bar0Base).toBe(newBarBase);

      // Contract v1 fixed BAR0 layout.
      const commonBase = bar0Base + 0x0000n;
      const notifyBase = bar0Base + 0x1000n;
      const isrBase = bar0Base + 0x2000n;

      // -------------------------------------------------------------------------------------------
      // Virtio modern init (feature negotiation).
      // -------------------------------------------------------------------------------------------
      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE);
      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

      for (const sel of [0, 1]) {
        mmioWriteU32(commonBase + 0x00n, sel);
        const f = mmioReadU32(commonBase + 0x04n);
        mmioWriteU32(commonBase + 0x08n, sel);
        mmioWriteU32(commonBase + 0x0cn, f);
      }

      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);
      mmioWriteU8(
        commonBase + 0x14n,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK,
      );
      expect(dev.driverOk()).toBe(true);

      // -------------------------------------------------------------------------------------------
      // Queue config (eventq = queue 0).
      // -------------------------------------------------------------------------------------------
      const desc = 0x1000;
      const avail = 0x2000;
      const used = 0x3000;
      const eventBufBase = 0x4000;

      mmioWriteU16(commonBase + 0x16n, 0);
      expect(mmioReadU16(commonBase + 0x18n)).toBe(64);
      expect(mmioReadU16(commonBase + 0x1en)).toBe(0);

      mmioWriteU64(commonBase + 0x20n, BigInt(desc));
      mmioWriteU64(commonBase + 0x28n, BigInt(avail));
      mmioWriteU64(commonBase + 0x30n, BigInt(used));
      mmioWriteU16(commonBase + 0x1cn, 1);

      const bufferCount = 16;
      for (let i = 0; i < bufferCount; i += 1) {
        const bufAddr = eventBufBase + i * 8;
        guestWriteBytes(bufAddr, new Uint8Array(8).fill(0xaa));
        guestWriteDesc(desc, i, bufAddr, 8, VIRTQ_DESC_F_WRITE, 0);
      }

      // Avail ring: flags=0, idx=bufferCount, ring[i]=descriptor index.
      guestWriteU16(avail + 0, 0);
      guestWriteU16(avail + 2, bufferCount);
      for (let i = 0; i < bufferCount; i += 1) {
        guestWriteU16(avail + 4 + i * 2, i);
      }
      // Used ring: flags=0, idx=0.
      guestWriteU16(used + 0, 0);
      guestWriteU16(used + 2, 0);
      for (let i = 0; i < bufferCount; i += 1) {
        guestWriteU32(used + 4 + i * 8 + 0, 0);
        guestWriteU32(used + 4 + i * 8 + 4, 0);
      }

      // Notify queue 0 (notify_off_multiplier is fixed to 4 in contract v1).
      mmioWriteU16(notifyBase + 0n, 0);
      expect(guestReadU16(used + 2)).toBe(0);

      // -------------------------------------------------------------------------------------------
      // Host injects relative motion / wheel / button transitions.
      // -------------------------------------------------------------------------------------------
      dev.injectRelMove(5, -3);
      expect(guestReadU16(used + 2)).toBe(3);

      // used[0..2]
      for (let i = 0; i < 3; i += 1) {
        expect(guestReadU32(used + 4 + i * 8 + 0)).toBe(i);
        expect(guestReadU32(used + 4 + i * 8 + 4)).toBe(8);
      }

      const ev0 = decodeEvent(guestReadBytes(eventBufBase + 0 * 8, 8));
      const ev1 = decodeEvent(guestReadBytes(eventBufBase + 1 * 8, 8));
      const ev2 = decodeEvent(guestReadBytes(eventBufBase + 2 * 8, 8));
      expect(ev0).toEqual({ type_: EV_REL, code: REL_X, value: 5 });
      expect(ev1).toEqual({ type_: EV_REL, code: REL_Y, value: -3 });
      expect(ev2).toEqual({ type_: EV_SYN, code: SYN_REPORT, value: 0 });

      dev.injectWheel(1);
      expect(guestReadU16(used + 2)).toBe(5);

      for (let i = 3; i < 5; i += 1) {
        expect(guestReadU32(used + 4 + i * 8 + 0)).toBe(i);
        expect(guestReadU32(used + 4 + i * 8 + 4)).toBe(8);
      }

      const ev3 = decodeEvent(guestReadBytes(eventBufBase + 3 * 8, 8));
      const ev4 = decodeEvent(guestReadBytes(eventBufBase + 4 * 8, 8));
      expect(ev3).toEqual({ type_: EV_REL, code: REL_WHEEL, value: 1 });
      expect(ev4).toEqual({ type_: EV_SYN, code: SYN_REPORT, value: 0 });

      dev.injectMouseButtons(0x01);
      expect(guestReadU16(used + 2)).toBe(7);
      const ev5 = decodeEvent(guestReadBytes(eventBufBase + 5 * 8, 8));
      const ev6 = decodeEvent(guestReadBytes(eventBufBase + 6 * 8, 8));
      expect(ev5).toEqual({ type_: EV_KEY, code: BTN_LEFT, value: 1 });
      expect(ev6).toEqual({ type_: EV_SYN, code: SYN_REPORT, value: 0 });

      dev.injectMouseButtons(0x00);
      expect(guestReadU16(used + 2)).toBe(9);
      const ev7 = decodeEvent(guestReadBytes(eventBufBase + 7 * 8, 8));
      const ev8 = decodeEvent(guestReadBytes(eventBufBase + 8 * 8, 8));
      expect(ev7).toEqual({ type_: EV_KEY, code: BTN_LEFT, value: 0 });
      expect(ev8).toEqual({ type_: EV_SYN, code: SYN_REPORT, value: 0 });

      // Queue interrupt should assert INTx.
      expect(irqState.raised).toBeGreaterThan(0);
      expect(irqState.asserted).toBe(true);

      // Reading ISR must acknowledge and deassert INTx.
      const isr = mmioReadU8(isrBase);
      expect(isr & 0x01).toBe(0x01);
      expect(irqState.lowered).toBeGreaterThan(0);
      expect(irqState.asserted).toBe(false);
    } finally {
      try {
        dev.destroy();
      } catch {
        // ignore
      }
      void pciAddr;
    }
  });
});
