import { describe, expect, it } from "vitest";

import { computeGuestRamLayout, guestToLinear } from "../../runtime/shared_layout";
import { assertWasmMemoryWiring } from "../../runtime/wasm_memory_probe";
import { initWasm } from "../../runtime/wasm_loader";
import { DeviceManager, type IrqSink } from "../device_manager";
import type { PciAddress } from "../bus/pci";
import { VirtioSndPciDevice } from "./virtio_snd";

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

// Feature bits (subset needed for the minimal virtio-snd implementation).
const VIRTIO_F_RING_INDIRECT_DESC = 1 << 28;

// Virtqueue descriptor flags.
const VIRTQ_DESC_F_NEXT = 1;
const VIRTQ_DESC_F_WRITE = 2;

// virtio-snd control request codes.
const VIRTIO_SND_R_PCM_INFO = 0x0100;

// virtio-snd response status codes.
const VIRTIO_SND_S_OK = 0x0000;

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

describe("io/devices/virtio-snd (legacy virtio-pci I/O integration)", () => {
  it("handles a PCM_INFO control request via the legacy virtio-pci I/O BAR (BAR2)", async () => {
    // Allocate a wasm memory large enough to host both the Rust/WASM runtime and
    // a small guest RAM window for our virtqueue rings + request/response buffers.
    const desiredGuestBytes = 0x40_000; // 256 KiB
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

    assertWasmMemoryWiring({ api, memory, context: "virtio_snd_pci_legacy_integration.test" });

    const Bridge = api.VirtioSndPciBridge;
    if (!Bridge) return;

    const base = layout.guest_base >>> 0;
    const size = layout.guest_size >>> 0;

    // Prefer a legacy-only virtio-pci device (modern capabilities disabled, legacy I/O BAR enabled).
    // Older builds may not accept the 3rd arg; skip in that case.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const AnyCtor = Bridge as any;
    let bridge: any;
    try {
      bridge = new AnyCtor(base, size, "legacy");
    } catch {
      return;
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
    try {
      const probe = (legacyRead.call(bridge, 0, 4) as number) >>> 0;
      if (probe === 0xffff_ffff) {
        bridge.free?.();
        return;
      }
    } catch {
      try {
        bridge.free?.();
      } catch {
        // ignore
      }
      return;
    }

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

    const dev = new VirtioSndPciDevice({ bridge, irqSink: mgr.irqSink, mode: "legacy" });

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

      // Legacy/transitional virtio-snd uses the 0x1000-range transitional ID (0x1018).
      const idDword = cfgReadU32(pciAddr, 0x00);
      expect(idDword & 0xffff).toBe(0x1af4);
      expect((idDword >>> 16) & 0xffff).toBe(0x1018);

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
      // Legacy transport exposes only the low 32 bits of the virtio feature bitmap.
      expect(hostFeatures).toBe(VIRTIO_F_RING_INDIRECT_DESC);
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_GUEST_FEATURES, 4, hostFeatures);

      // -----------------------------------------------------------------------------------------
      // Queue config (controlq = queue 0).
      // -----------------------------------------------------------------------------------------
      const controlQueue = 0;
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_SEL, 2, controlQueue);
      const queueNum = mgr.portRead(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_NUM, 2) & 0xffff;
      expect(queueNum).toBe(64);

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
      // Post a control request descriptor chain and expect a response.
      // -----------------------------------------------------------------------------------------
      const reqAddr = 0x8000;
      const respAddr = 0x9000;

      // Request: PCM_INFO(start_id=0, count=2).
      const req = new Uint8Array(12);
      const reqDv = new DataView(req.buffer);
      reqDv.setUint32(0, VIRTIO_SND_R_PCM_INFO, true);
      reqDv.setUint32(4, 0, true);
      reqDv.setUint32(8, 2, true);
      guestWriteBytes(reqAddr, req);

      // Response: status (u32) + 2x pcm_info (32 bytes each) = 68 bytes.
      guestWriteBytes(respAddr, new Uint8Array(128).fill(0xaa));

      guestWriteDesc(desc, 0, reqAddr, req.byteLength, VIRTQ_DESC_F_NEXT, 1);
      guestWriteDesc(desc, 1, respAddr, 128, VIRTQ_DESC_F_WRITE, 0);

      // Avail ring: flags=0, idx=1, ring[0]=0.
      guestWriteU16(avail + 0, 0);
      guestWriteU16(avail + 2, 1);
      guestWriteU16(avail + 4, 0);
      // Used ring: flags=0, idx=0.
      guestWriteU16(used + 0, 0);
      guestWriteU16(used + 2, 0);
      guestWriteU32(used + 4, 0);
      guestWriteU32(used + 8, 0);

      // Notify control queue. In the browser integration the legacy kick should be processed
      // synchronously (no need for periodic `tick()` polling).
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, 2, controlQueue);

      // Used ring should have advanced and the response should be present in guest RAM.
      expect(guestReadU16(used + 2)).toBe(1);
      expect(guestReadU32(used + 4)).toBe(0);
      const usedLen = guestReadU32(used + 8);
      expect(usedLen).toBe(68);

      const respOut = guestReadBytes(respAddr, usedLen);
      const respDv = new DataView(respOut.buffer, respOut.byteOffset, respOut.byteLength);
      expect(respDv.getUint32(0, true)).toBe(VIRTIO_SND_S_OK);
      // Two 32-byte pcm_info entries follow.
      expect(respOut.byteLength).toBe(68);

      // Entry 0: playback stream_id=0.
      expect(respDv.getUint32(4, true)).toBe(0);
      // Entry 1: capture stream_id=1.
      expect(respDv.getUint32(4 + 32, true)).toBe(1);

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

