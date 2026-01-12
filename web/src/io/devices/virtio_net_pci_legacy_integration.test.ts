import { describe, expect, it } from "vitest";

import { openRingByKind } from "../../ipc/ipc";
import { createIoIpcSab, computeGuestRamLayout, guestToLinear, IO_IPC_NET_TX_QUEUE_KIND } from "../../runtime/shared_layout";
import { assertWasmMemoryWiring } from "../../runtime/wasm_memory_probe";
import { initWasm } from "../../runtime/wasm_loader";
import { DeviceManager, type IrqSink } from "../device_manager";
import type { PciAddress } from "../bus/pci";
import { VirtioNetPciDevice } from "./virtio_net";

// Legacy virtio-pci (0.9) I/O port register layout (see `crates/aero-virtio/src/pci.rs`).
const VIRTIO_PCI_LEGACY_HOST_FEATURES = 0x00; // u32
const VIRTIO_PCI_LEGACY_GUEST_FEATURES = 0x04; // u32
const VIRTIO_PCI_LEGACY_QUEUE_PFN = 0x08; // u32
const VIRTIO_PCI_LEGACY_QUEUE_NUM = 0x0c; // u16
const VIRTIO_PCI_LEGACY_QUEUE_SEL = 0x0e; // u16
const VIRTIO_PCI_LEGACY_QUEUE_NOTIFY = 0x10; // u16
const VIRTIO_PCI_LEGACY_STATUS = 0x12; // u8

// Virtio status flags.
const VIRTIO_STATUS_ACKNOWLEDGE = 1;
const VIRTIO_STATUS_DRIVER = 2;
const VIRTIO_STATUS_DRIVER_OK = 4;

// Feature bits (subset required by the Aero virtio-net contract v1).
const VIRTIO_NET_F_MAC = 1 << 5;
const VIRTIO_NET_F_STATUS = 1 << 16;
const VIRTIO_F_RING_INDIRECT_DESC = 1 << 28;

// Virtqueue descriptor flags.
const VIRTQ_DESC_F_NEXT = 1;

// `struct virtio_net_hdr` base length (see `crates/aero-virtio/src/devices/net_offload.rs`).
const VIRTIO_NET_HDR_LEN = 10;

function cfgAddr(addr: PciAddress, off: number): number {
  return (0x8000_0000 | ((addr.bus & 0xff) << 16) | ((addr.device & 0x1f) << 11) | ((addr.function & 0x07) << 8) | (off & 0xfc)) >>> 0;
}

function alignUp(value: number, align: number): number {
  return (value + align - 1) & ~(align - 1);
}

describe("io/devices/virtio-net (pci bridge integration)", () => {
  it("TX frames cross NET_TX via virtio-pci legacy I/O transport", async () => {
    // Allocate a wasm memory large enough to host both the Rust/WASM runtime and
    // a small guest RAM window for our virtqueue rings + test buffers.
    const desiredGuestBytes = 0x20_000; // 128 KiB
    const layout = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layout.wasm_pages, maximum: layout.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // The wasm-pack output is generated and may be absent in some test
      // environments; skip rather than failing unrelated suites.
      if (message.includes("Missing single-thread WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "virtio_net_pci_legacy_integration.test" });

    const Bridge = api.VirtioNetPciBridge;
    if (!Bridge) return;

    const ioIpcSab = createIoIpcSab();
    const netTxRing = openRingByKind(ioIpcSab, IO_IPC_NET_TX_QUEUE_KIND, 0);
    netTxRing.reset();

    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    // Legacy-only virtio-pci device (modern capabilities disabled, legacy I/O BAR enabled).
    const bridge = new Bridge(layout.guest_base >>> 0, layout.guest_size >>> 0, ioIpcSab, "legacy");
    const dev = new VirtioNetPciDevice({ bridge, irqSink: mgr.irqSink, mode: "legacy" });

    const dv = new DataView(memory.buffer);

    const guestWriteU16 = (paddr: number, value: number) => dv.setUint16(guestToLinear(layout, paddr), value & 0xffff, true);
    const guestWriteU32 = (paddr: number, value: number) => dv.setUint32(guestToLinear(layout, paddr), value >>> 0, true);
    const guestReadU16 = (paddr: number) => dv.getUint16(guestToLinear(layout, paddr), true) >>> 0;
    const guestReadU32 = (paddr: number) => dv.getUint32(guestToLinear(layout, paddr), true) >>> 0;
    const guestWriteBytes = (paddr: number, bytes: Uint8Array) => {
      new Uint8Array(memory.buffer, guestToLinear(layout, paddr), bytes.byteLength).set(bytes);
    };
    const guestWriteDesc = (table: number, index: number, addr: number, len: number, flags: number, next: number) => {
      const base = table + index * 16;
      // u64 addr
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

      // Basic PCI identification (legacy/transitional virtio-net uses device ID 0x1000).
      const idDword = cfgReadU32(pciAddr, 0x00);
      expect(idDword & 0xffff).toBe(0x1af4);
      expect((idDword >>> 16) & 0xffff).toBe(0x1000);

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
      cfgWriteU32(pciAddr, 0x04, (cmdBefore | 0x1) >>> 0);
      const cmdAfter = cfgReadU16(pciAddr, 0x04);
      expect((cmdAfter & 0x0001) !== 0).toBe(true);

      // ---------------------------------------------------------------------------------------
      // Virtio legacy init (feature negotiation).
      // ---------------------------------------------------------------------------------------
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_STATUS, 1, VIRTIO_STATUS_ACKNOWLEDGE);
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_STATUS, 1, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

      const hostFeatures = mgr.portRead(bar2Base + VIRTIO_PCI_LEGACY_HOST_FEATURES, 4) >>> 0;
      expect((hostFeatures & VIRTIO_NET_F_MAC) !== 0).toBe(true);
      expect((hostFeatures & VIRTIO_NET_F_STATUS) !== 0).toBe(true);
      expect((hostFeatures & VIRTIO_F_RING_INDIRECT_DESC) !== 0).toBe(true);
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_GUEST_FEATURES, 4, hostFeatures);

      // ---------------------------------------------------------------------------------------
      // Queue config (TX=queue1).
      // ---------------------------------------------------------------------------------------
      const txQueue = 1;
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_SEL, 2, txQueue);
      const queueNum = mgr.portRead(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_NUM, 2) & 0xffff;
      expect(queueNum).toBe(256);

      // Legacy ring base must be 4KiB-aligned. `QUEUE_PFN` takes the physical page frame number.
      const txRingBase = 0x4000;
      expect((txRingBase & 0xfff) === 0).toBe(true);
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_PFN, 4, txRingBase >>> 12);

      // ---------------------------------------------------------------------------------------
      // Bring device to DRIVER_OK.
      // ---------------------------------------------------------------------------------------
      mgr.portWrite(
        bar2Base + VIRTIO_PCI_LEGACY_STATUS,
        1,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
      );

      // Legacy vring layout (mirrors `crates/aero-virtio/src/pci.rs::legacy_vring_addresses`).
      const txDesc = txRingBase;
      const txAvail = txDesc + 16 * queueNum;
      const txUsedUnaligned = txAvail + 4 + 2 * queueNum + 2;
      const txUsed = alignUp(txUsedUnaligned, 4096);

      // ---------------------------------------------------------------------------------------
      // TX: post a descriptor chain (virtio_net_hdr + Ethernet payload) and expect NET_TX output.
      // ---------------------------------------------------------------------------------------
      const hdrAddr = 0x7000;
      const payloadAddr = 0x7100;
      const hdr = new Uint8Array(VIRTIO_NET_HDR_LEN); // all zeros
      const txFrame = new Uint8Array([0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0x08, 0x00]);
      guestWriteBytes(hdrAddr, hdr);
      guestWriteBytes(payloadAddr, txFrame);

      guestWriteDesc(txDesc, 0, hdrAddr, hdr.byteLength, VIRTQ_DESC_F_NEXT, 1);
      guestWriteDesc(txDesc, 1, payloadAddr, txFrame.byteLength, 0, 0);

      // Avail ring: flags=0, idx=1, ring[0]=0.
      guestWriteU16(txAvail + 0, 0);
      guestWriteU16(txAvail + 2, 1);
      guestWriteU16(txAvail + 4, 0);
      // Used ring: flags=0, idx=0.
      guestWriteU16(txUsed + 0, 0);
      guestWriteU16(txUsed + 2, 0);
      guestWriteU32(txUsed + 4, 0);
      guestWriteU32(txUsed + 8, 0);

      // Notify TX queue. In the browser integration the legacy kick should be processed
      // synchronously (no need for periodic `tick()` polling).
      mgr.portWrite(bar2Base + VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, 2, txQueue);

      const popped = netTxRing.tryPop();
      expect(popped).not.toBeNull();
      expect(Array.from(popped!)).toEqual(Array.from(txFrame));
      expect(netTxRing.tryPop()).toBeNull();

      // Used ring should have advanced.
      expect(guestReadU16(txUsed + 2)).toBe(1);
      expect(guestReadU32(txUsed + 4)).toBe(0);
      expect(guestReadU32(txUsed + 8)).toBe(0);
    } finally {
      try {
        dev.destroy();
      } catch {
        // ignore
      }
    }
  });
});
