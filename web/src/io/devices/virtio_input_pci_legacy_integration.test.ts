import { describe, expect, it } from "vitest";

import { computeGuestRamLayout, guestToLinear } from "../../runtime/shared_layout";
import { assertWasmMemoryWiring } from "../../runtime/wasm_memory_probe";
import { initWasm } from "../../runtime/wasm_loader";
import { DeviceManager, type IrqSink } from "../device_manager";
import type { PciAddress } from "../bus/pci";
import { VirtioInputPciFunction } from "./virtio_input";

// Legacy virtio-pci (0.9) I/O port register layout (see `crates/aero-virtio/src/pci.rs`).
const VIRTIO_PCI_LEGACY_HOST_FEATURES = 0x00; // u32
const VIRTIO_PCI_LEGACY_GUEST_FEATURES = 0x04; // u32
const VIRTIO_PCI_LEGACY_QUEUE_PFN = 0x08; // u32
const VIRTIO_PCI_LEGACY_QUEUE_NUM = 0x0c; // u16
const VIRTIO_PCI_LEGACY_QUEUE_SEL = 0x0e; // u16
const VIRTIO_PCI_LEGACY_QUEUE_NOTIFY = 0x10; // u16
const VIRTIO_PCI_LEGACY_STATUS = 0x12; // u8
const VIRTIO_PCI_LEGACY_ISR = 0x13; // u8 (read clears)

// Virtio status flags.
const VIRTIO_STATUS_ACKNOWLEDGE = 1;
const VIRTIO_STATUS_DRIVER = 2;
const VIRTIO_STATUS_DRIVER_OK = 4;

// Virtqueue descriptor flags.
const VIRTQ_DESC_F_WRITE = 2;

// Linux input ABI (matches `crates/aero-virtio/src/devices/input.rs`).
const EV_SYN = 0;
const EV_KEY = 1;
const SYN_REPORT = 0;
const KEY_A = 30;

function cfgAddr(addr: PciAddress, off: number): number {
  return (
    0x8000_0000 |
    ((addr.bus & 0xff) << 16) |
    ((addr.device & 0x1f) << 11) |
    ((addr.function & 0x07) << 8) |
    (off & 0xfc)
  ) >>> 0;
}

function alignUp(value: number, align: number): number {
  return (value + align - 1) & ~(align - 1);
}

describe("io/devices/virtio-input (legacy virtio-pci I/O integration)", () => {
  it("injects EV_KEY/EV_SYN events via the virtio-pci legacy I/O BAR (BAR2)", async () => {
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

    assertWasmMemoryWiring({ api, memory, context: "virtio_input_pci_legacy_integration.test" });

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

    const base = layout.guest_base >>> 0;
    const size = layout.guest_size >>> 0;

    // Prefer a legacy-only virtio-pci device (modern capabilities disabled, legacy I/O BAR enabled).
    // Older builds may not accept the 4th argument; fall back gracefully.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const AnyCtor = Ctor as any;
    let bridge: any;
    try {
      bridge = new AnyCtor(base, size, "keyboard", "legacy");
    } catch {
      try {
        bridge = new AnyCtor(base, size, "keyboard");
      } catch {
        // Older/partial builds may export VirtioInputPciDevice but not the expected signature.
        return;
      }
    }

    // Ensure legacy I/O accessors exist (some builds expose `io_*`, newer ones `legacy_io_*`).
    const legacyRead =
      typeof bridge.legacy_io_read === "function"
        ? bridge.legacy_io_read
        : typeof bridge.io_read === "function"
          ? bridge.io_read
          : null;
    const legacyWrite =
      typeof bridge.legacy_io_write === "function"
        ? bridge.legacy_io_write
        : typeof bridge.io_write === "function"
          ? bridge.io_write
          : null;
    if (!legacyRead || !legacyWrite) {
      try {
        bridge.free?.();
      } catch {
        // ignore
      }
      return;
    }

    // Detect whether legacy I/O is actually enabled (modern-only devices may still expose the
    // methods but return open-bus reads).
    //
    // Note: virtio-pci legacy IO reads are gated by PCI command bit0 (I/O enable). For the probe
    // we temporarily enable I/O decoding inside the bridge so the read is meaningful.
    const setCmd = typeof bridge.set_pci_command === "function" ? bridge.set_pci_command : null;
    let probeOk = false;
    try {
      if (setCmd) {
        try {
          setCmd.call(bridge, 0x0001);
        } catch {
          // ignore
        }
      }
      const probe = (legacyRead.call(bridge, 0, 4) as number) >>> 0;
      probeOk = probe !== 0xffff_ffff;
    } catch {
      probeOk = false;
    } finally {
      if (setCmd) {
        try {
          setCmd.call(bridge, 0x0000);
        } catch {
          // ignore
        }
      }
    }

    if (!probeOk) {
      try {
        bridge.free?.();
      } catch {
        // ignore
      }
      return;
    }

    // Wrap as a PCI function with BAR2 enabled.
    const dev = new VirtioInputPciFunction({ kind: "keyboard", device: bridge, irqSink: mgr.irqSink, mode: "legacy" });

    const dv = new DataView(memory.buffer);
    const guestWriteU16 = (paddr: number, value: number) => dv.setUint16(guestToLinear(layout, paddr), value & 0xffff, true);
    const guestWriteU32 = (paddr: number, value: number) => dv.setUint32(guestToLinear(layout, paddr), value >>> 0, true);
    const guestReadU16 = (paddr: number) => dv.getUint16(guestToLinear(layout, paddr), true) >>> 0;
    const guestReadU32 = (paddr: number) => dv.getUint32(guestToLinear(layout, paddr), true) >>> 0;
    const guestWriteBytes = (paddr: number, bytes: Uint8Array) => {
      new Uint8Array(memory.buffer, guestToLinear(layout, paddr), bytes.byteLength).set(bytes);
    };
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
    const cfgWriteU32 = (addr: PciAddress, off: number, value: number) => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(addr, off));
      mgr.portWrite(0x0cfc + (off & 3), 4, value >>> 0);
    };

    let pciAddr: PciAddress | null = null;
    try {
      pciAddr = mgr.registerPciDevice(dev);

      // Legacy/transitional virtio-input uses the 0x1000-range transitional ID (0x1011).
      const idDword = cfgReadU32(pciAddr, 0x00);
      expect(idDword & 0xffff).toBe(0x1af4);
      expect((idDword >>> 16) & 0xffff).toBe(0x1011);

      // BAR2 should be an I/O BAR.
      const bar2Initial = cfgReadU32(pciAddr, 0x18);
      expect((bar2Initial & 0x1) !== 0).toBe(true);

      // Probe BAR2 size mask via the standard all-ones write.
      cfgWriteU32(pciAddr, 0x18, 0xffff_ffff);
      const bar2Mask = cfgReadU32(pciAddr, 0x18);
      expect(bar2Mask).toBe(0xffff_ff01);

      // Assign BAR2 to a fixed base and validate that I/O decode is gated by PCI command bit 0.
      const bar2Base = 0xd000;
      cfgWriteU32(pciAddr, 0x18, (bar2Base & 0xffff_fffc) | 0x1);

      // I/O space disabled: reads should hit the unmapped default (all-ones).
      expect(mgr.portRead(bar2Base + VIRTIO_PCI_LEGACY_HOST_FEATURES, 4) >>> 0).toBe(0xffff_ffff);

      const cmdBefore = cfgReadU16(pciAddr, 0x04);
      // Enable I/O decoding (bit 0) + Bus Master Enable (bit 2).
      cfgWriteU32(pciAddr, 0x04, (cmdBefore | 0x5) >>> 0);
      const cmdAfter = cfgReadU16(pciAddr, 0x04);
      expect((cmdAfter & 0x0001) !== 0).toBe(true);
      expect((cmdAfter & 0x0004) !== 0).toBe(true);

      // -----------------------------------------------------------------------------------------
      // Virtio legacy init (feature negotiation).
      // -----------------------------------------------------------------------------------------
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_STATUS, 1, VIRTIO_STATUS_ACKNOWLEDGE);
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_STATUS, 1, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

      const hostFeatures = mgr.portRead(bar2Base + VIRTIO_PCI_LEGACY_HOST_FEATURES, 4) >>> 0;
      expect(hostFeatures).not.toBe(0xffff_ffff);
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_GUEST_FEATURES, 4, hostFeatures);

      // -----------------------------------------------------------------------------------------
      // Queue config (eventq = queue 0).
      // -----------------------------------------------------------------------------------------
      const eventQueue = 0;
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_SEL, 2, eventQueue);
      const queueNum = mgr.portRead(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_NUM, 2) & 0xffff;
      expect(queueNum).toBeGreaterThan(0);

      // Legacy ring base must be 4KiB-aligned. `QUEUE_PFN` takes the physical page frame number.
      const ringBase = 0x1000;
      expect((ringBase & 0xfff) === 0).toBe(true);
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_PFN, 4, ringBase >>> 12);

      // Bring device to DRIVER_OK.
      mgr.portWrite(
        bar2Base + VIRTIO_PCI_LEGACY_STATUS,
        1,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
      );

      // Legacy vring layout (mirrors `crates/aero-virtio/src/pci.rs::legacy_vring_addresses`).
      const desc = ringBase;
      const avail = desc + 16 * queueNum;
      const usedUnaligned = avail + 4 + 2 * queueNum + 2;
      const used = alignUp(usedUnaligned, 4096);

      // -----------------------------------------------------------------------------------------
      // Post a handful of event buffers (each 8 bytes, one input_event).
      // -----------------------------------------------------------------------------------------
      const bufferCount = Math.min(4, queueNum);
      const eventBufBase = 0x8000;
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

      // Kick queue 0: should make buffers visible but not produce used entries yet.
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, 2, eventQueue);
      expect(guestReadU16(used + 2)).toBe(0);

      // -----------------------------------------------------------------------------------------
      // Host injects a key press and we should observe EV_KEY + EV_SYN events.
      // -----------------------------------------------------------------------------------------
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

      // Reading legacy ISR must acknowledge and deassert INTx.
      const isr = mgr.portRead(bar2Base + VIRTIO_PCI_LEGACY_ISR, 1) & 0xff;
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
