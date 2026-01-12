import { describe, expect, it } from "vitest";

import { openRingByKind } from "../../ipc/ipc";
import type { RingBuffer } from "../../ipc/ring_buffer";
import { createIoIpcSab, computeGuestRamLayout, guestToLinear, IO_IPC_NET_RX_QUEUE_KIND, IO_IPC_NET_TX_QUEUE_KIND } from "../../runtime/shared_layout";
import { initWasm } from "../../runtime/wasm_loader";
import { DeviceManager, type IrqSink } from "../device_manager";
import type { PciAddress } from "../bus/pci";
import { VirtioNetPciDevice } from "./virtio_net";

const PCI_CAP_ID_VENDOR_SPECIFIC = 0x09;

// Virtio PCI capability types (see `crates/aero-virtio/src/pci.rs`).
const VIRTIO_PCI_CAP_COMMON_CFG = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG = 2;
const VIRTIO_PCI_CAP_ISR_CFG = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG = 4;

// Virtio status flags (see virtio spec).
const VIRTIO_STATUS_ACKNOWLEDGE = 1;
const VIRTIO_STATUS_DRIVER = 2;
const VIRTIO_STATUS_DRIVER_OK = 4;
const VIRTIO_STATUS_FEATURES_OK = 8;

// Virtqueue descriptor flags.
const VIRTQ_DESC_F_NEXT = 1;
const VIRTQ_DESC_F_WRITE = 2;

// `struct virtio_net_hdr` base length (see `crates/aero-virtio/src/devices/net_offload.rs`).
const VIRTIO_NET_HDR_LEN = 10;

function cfgAddr(addr: PciAddress, off: number): number {
  return (0x8000_0000 | ((addr.bus & 0xff) << 16) | ((addr.device & 0x1f) << 11) | ((addr.function & 0x07) << 8) | (off & 0xfc)) >>> 0;
}

function writeU32LE(buf: Uint8Array, off: number, value: number): void {
  buf[off] = value & 0xff;
  buf[off + 1] = (value >>> 8) & 0xff;
  buf[off + 2] = (value >>> 16) & 0xff;
  buf[off + 3] = (value >>> 24) & 0xff;
}

function readU32LE(buf: Uint8Array, off: number): number {
  return (buf[off]! | (buf[off + 1]! << 8) | (buf[off + 2]! << 16) | (buf[off + 3]! << 24)) >>> 0;
}

type VirtioPciCaps = {
  commonOff: number | null;
  notifyOff: number | null;
  notifyMult: number | null;
  isrOff: number | null;
  deviceOff: number | null;
};

function parseVirtioPciCaps(cfg: Uint8Array): VirtioPciCaps {
  // Capabilities pointer.
  let ptr = cfg[0x34] ?? 0;
  const caps: VirtioPciCaps = {
    commonOff: null,
    notifyOff: null,
    notifyMult: null,
    isrOff: null,
    deviceOff: null,
  };

  // Guard against malformed/cyclic lists.
  const seen = new Set<number>();
  while (ptr !== 0) {
    if (ptr >= 0x100) throw new Error(`PCI cap pointer out of range: 0x${ptr.toString(16)}`);
    if (seen.has(ptr)) throw new Error(`PCI cap list cycle detected at 0x${ptr.toString(16)}`);
    seen.add(ptr);

    const capId = cfg[ptr]!;
    const next = cfg[ptr + 1]! >>> 0;
    const capLen = cfg[ptr + 2]! >>> 0;

    if (capId === PCI_CAP_ID_VENDOR_SPECIFIC) {
      // `struct virtio_pci_cap` (virtio spec):
      //  0 cap_vndr (0x09)
      //  1 cap_next
      //  2 cap_len
      //  3 cfg_type
      //  4 bar
      //  8..11  offset (le32)
      // 12..15  length (le32)
      // notify has extra le32 at 16..19: notify_off_multiplier
      if (capLen < 16) throw new Error(`virtio_pci_cap too short: len=${capLen}`);
      const cfgType = cfg[ptr + 3]! >>> 0;
      const bar = cfg[ptr + 4]! >>> 0;
      if (bar !== 0) throw new Error(`virtio_pci_cap BAR != 0 (got ${bar}); test expects BAR0-backed caps`);
      const offset = readU32LE(cfg, ptr + 8);

      switch (cfgType) {
        case VIRTIO_PCI_CAP_COMMON_CFG:
          caps.commonOff = offset;
          break;
        case VIRTIO_PCI_CAP_NOTIFY_CFG:
          caps.notifyOff = offset;
          if (capLen < 20) throw new Error(`virtio notify cap too short: len=${capLen}`);
          caps.notifyMult = readU32LE(cfg, ptr + 16);
          break;
        case VIRTIO_PCI_CAP_ISR_CFG:
          caps.isrOff = offset;
          break;
        case VIRTIO_PCI_CAP_DEVICE_CFG:
          caps.deviceOff = offset;
          break;
        default:
          break;
      }
    }

    ptr = next;
  }

  return caps;
}

describe("io/devices/virtio-net (pci bridge integration)", () => {
  it("TX and RX frames cross NET_TX/NET_RX via virtio-pci modern transport", async () => {
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

    // Older/partial builds may not yet include the virtio-net bridge export.
    const Bridge = api.VirtioNetPciBridge;
    if (!Bridge) return;

    const ioIpcSab = createIoIpcSab();
    const netTxRing = openRingByKind(ioIpcSab, IO_IPC_NET_TX_QUEUE_KIND, 0);
    const netRxRing = openRingByKind(ioIpcSab, IO_IPC_NET_RX_QUEUE_KIND, 0);
    // Ensure rings start empty (fresh SABs should already be empty, but be explicit).
    netTxRing.reset();
    netRxRing.reset();

    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    const bridge = new Bridge(layout.guest_base >>> 0, layout.guest_size >>> 0, ioIpcSab);
    const dev = new VirtioNetPciDevice({ bridge, irqSink: mgr.irqSink });

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
      // u64 addr
      dv.setUint32(guestToLinear(layout, base), addr >>> 0, true);
      dv.setUint32(guestToLinear(layout, base + 4), 0, true);
      dv.setUint32(guestToLinear(layout, base + 8), len >>> 0, true);
      dv.setUint16(guestToLinear(layout, base + 12), flags & 0xffff, true);
      dv.setUint16(guestToLinear(layout, base + 14), next & 0xffff, true);
    };

    const mmioReadU16 = (addr: bigint) => mgr.mmioRead(addr, 2) & 0xffff;
    const mmioReadU32 = (addr: bigint) => mgr.mmioRead(addr, 4) >>> 0;
    const mmioWriteU8 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 1, value & 0xff);
    const mmioWriteU16 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 2, value & 0xffff);
    const mmioWriteU32 = (addr: bigint, value: number) => mgr.mmioWrite(addr, 4, value >>> 0);
    const mmioWriteU64 = (addr: bigint, value: bigint) => {
      mmioWriteU32(addr, Number(value & 0xffff_ffffn));
      mmioWriteU32(addr + 4n, Number((value >> 32n) & 0xffff_ffffn));
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

    let pciAddr: PciAddress | null = null;
    try {
      pciAddr = mgr.registerPciDevice(dev);

      // Read BAR0 and ensure it's a 64-bit memory BAR.
      const bar0LowInitial = cfgReadU32(pciAddr, 0x10);
      void cfgReadU32(pciAddr, 0x14);
      // Bits 2:1 = 0b10 indicates a 64-bit memory BAR.
      expect(bar0LowInitial & 0x6).toBe(0x4);

      // Force BAR0 above 4GiB so we exercise the high dword plumbing.
      const newBarBase = 0x1_0000_0000n; // 4GiB
      const barAttrBits = bar0LowInitial & 0x0f;
      const newBar0Low = ((Number(newBarBase & 0xffff_ffffn) & 0xffff_fff0) | barAttrBits) >>> 0;
      const newBar0High = Number((newBarBase >> 32n) & 0xffff_ffffn) >>> 0;
      cfgWriteU32(pciAddr, 0x10, newBar0Low);
      cfgWriteU32(pciAddr, 0x14, newBar0High);

      // Enable PCI memory decoding (Command register bit 1).
      const cmd = cfgReadU16(pciAddr, 0x04);
      cfgWriteU16(pciAddr, 0x04, cmd | 0x2);

      // Compute mapped MMIO base.
      const bar0Low = cfgReadU32(pciAddr, 0x10);
      const bar0High = cfgReadU32(pciAddr, 0x14);
      const bar0Base = (BigInt(bar0High) << 32n) | BigInt(bar0Low & 0xffff_fff0);

      // Read full PCI config space (for capability parsing).
      const cfg = new Uint8Array(256);
      for (let off = 0; off < 256; off += 4) {
        writeU32LE(cfg, off, cfgReadU32(pciAddr, off));
      }

      const caps = parseVirtioPciCaps(cfg);
      // Note: common config is at offset 0x0000 in the Aero virtio-net PCI contract,
      // so we must treat `0` as a valid offset (use `null` as the "not found" sentinel).
      expect(caps.commonOff).not.toBeNull();
      expect(caps.notifyOff).not.toBeNull();
      expect(caps.isrOff).not.toBeNull();
      expect(caps.deviceOff).not.toBeNull();
      expect(caps.notifyMult).not.toBeNull();
      expect(caps.notifyMult).not.toBe(0);

      const commonBase = bar0Base + BigInt(caps.commonOff!);
      const notifyBase = bar0Base + BigInt(caps.notifyOff!);
      const deviceBase = bar0Base + BigInt(caps.deviceOff!);

      // -----------------------------------------------------------------------------------------
      // Virtio modern init (feature negotiation).
      // -----------------------------------------------------------------------------------------
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

      // -----------------------------------------------------------------------------------------
      // Queue config (RX=queue0, TX=queue1).
      // -----------------------------------------------------------------------------------------
      const rxDesc = 0x1000;
      const rxAvail = 0x2000;
      const rxUsed = 0x3000;
      const txDesc = 0x4000;
      const txAvail = 0x5000;
      const txUsed = 0x6000;

      const configureQueue = (queueIndex: number, desc: number, avail: number, used: number): number => {
        mmioWriteU16(commonBase + 0x16n, queueIndex);
        const max = mmioReadU16(commonBase + 0x18n);
        expect(max).toBeGreaterThanOrEqual(256);
        // Driver-selected queue size.
        mmioWriteU16(commonBase + 0x18n, 256);
        mmioWriteU64(commonBase + 0x20n, BigInt(desc));
        mmioWriteU64(commonBase + 0x28n, BigInt(avail));
        mmioWriteU64(commonBase + 0x30n, BigInt(used));
        const notifyOff = mmioReadU16(commonBase + 0x1en);
        mmioWriteU16(commonBase + 0x1cn, 1);
        return notifyOff;
      };

      const rxNotifyOff = configureQueue(0, rxDesc, rxAvail, rxUsed);
      const txNotifyOff = configureQueue(1, txDesc, txAvail, txUsed);

      // -----------------------------------------------------------------------------------------
      // Device config (sanity: ensure MMIO mapping works).
      // -----------------------------------------------------------------------------------------
      const macLo = mmioReadU32(deviceBase);
      const macHi = mmioReadU16(deviceBase + 4n);
      const mac = [
        macLo & 0xff,
        (macLo >>> 8) & 0xff,
        (macLo >>> 16) & 0xff,
        (macLo >>> 24) & 0xff,
        macHi & 0xff,
        (macHi >>> 8) & 0xff,
      ];
      expect(mac.some((b) => b !== 0)).toBe(true);

      // -----------------------------------------------------------------------------------------
      // TX: post a descriptor chain (virtio_net_hdr + Ethernet payload) and expect NET_TX output.
      // -----------------------------------------------------------------------------------------
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

      // Notify queue 1.
      const txNotifyAddr = notifyBase + BigInt(txNotifyOff) * BigInt(caps.notifyMult!);
      mmioWriteU16(txNotifyAddr, 1);
      dev.tick(0);

      const popped = waitPop(netTxRing);
      expect(popped).not.toBeNull();
      expect(Array.from(popped!)).toEqual(Array.from(txFrame));

      expect(guestReadU16(txUsed + 2)).toBe(1);
      expect(guestReadU32(txUsed + 8)).toBe(0);

      // -----------------------------------------------------------------------------------------
      // RX: push a frame into NET_RX, post an RX buffer chain, and expect guest RAM filled.
      // -----------------------------------------------------------------------------------------
      const rxFrame = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x08, 0x00]);
      expect(netRxRing.tryPush(rxFrame)).toBe(true);

      const rxHdrAddr = 0x7200;
      const rxPayloadAddr = 0x7300;
      guestWriteBytes(rxHdrAddr, new Uint8Array(VIRTIO_NET_HDR_LEN).fill(0xaa));
      guestWriteBytes(rxPayloadAddr, new Uint8Array(64).fill(0xbb));

      guestWriteDesc(rxDesc, 0, rxHdrAddr, VIRTIO_NET_HDR_LEN, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 1);
      guestWriteDesc(rxDesc, 1, rxPayloadAddr, 64, VIRTQ_DESC_F_WRITE, 0);

      guestWriteU16(rxAvail + 0, 0);
      guestWriteU16(rxAvail + 2, 1);
      guestWriteU16(rxAvail + 4, 0);
      guestWriteU16(rxUsed + 0, 0);
      guestWriteU16(rxUsed + 2, 0);
      guestWriteU32(rxUsed + 4, 0);
      guestWriteU32(rxUsed + 8, 0);

      const rxNotifyAddr = notifyBase + BigInt(rxNotifyOff) * BigInt(caps.notifyMult!);
      mmioWriteU16(rxNotifyAddr, 0);

      // Some implementations may poll NET_RX on tick rather than directly on notify.
      for (let i = 0; i < 8 && guestReadU16(rxUsed + 2) !== 1; i++) {
        dev.tick(i);
      }

      expect(guestReadU16(rxUsed + 2)).toBe(1);
      expect(guestReadU32(rxUsed + 8)).toBe(VIRTIO_NET_HDR_LEN + rxFrame.byteLength);

      expect(Array.from(guestReadBytes(rxHdrAddr, VIRTIO_NET_HDR_LEN))).toEqual(Array.from(new Uint8Array(VIRTIO_NET_HDR_LEN)));
      expect(Array.from(guestReadBytes(rxPayloadAddr, rxFrame.byteLength))).toEqual(Array.from(rxFrame));
    } finally {
      try {
        dev.destroy();
      } catch {
        // ignore
      }
    }
  });
});

function waitPop(ring: RingBuffer, maxIters = 64): Uint8Array | null {
  for (let i = 0; i < maxIters; i++) {
    const msg = ring.tryPop();
    if (msg) return msg;
  }
  return null;
}
