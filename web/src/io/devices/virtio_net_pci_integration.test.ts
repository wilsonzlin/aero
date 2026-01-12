import { describe, expect, it } from "vitest";

import { openRingByKind } from "../../ipc/ipc";
import { createIoIpcSab, computeGuestRamLayout, guestToLinear, IO_IPC_NET_RX_QUEUE_KIND, IO_IPC_NET_TX_QUEUE_KIND } from "../../runtime/shared_layout";
import { assertWasmMemoryWiring } from "../../runtime/wasm_memory_probe";
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

// Feature bits (subset required by the Aero virtio-net contract v1).
const VIRTIO_NET_F_MAC = 1 << 5;
const VIRTIO_NET_F_STATUS = 1 << 16;
const VIRTIO_NET_F_MRG_RXBUF = 1 << 15;
const VIRTIO_NET_F_CSUM = 1 << 0;
const VIRTIO_F_RING_INDIRECT_DESC = 1 << 28;
// VIRTIO_F_VERSION_1 is bit 32 in the 64-bit feature set, i.e. bit 0 when `features_sel = 1`.
const VIRTIO_F_VERSION_1_SEL1_BIT = 1 << 0;

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
  commonLen: number | null;
  notifyOff: number | null;
  notifyLen: number | null;
  notifyMult: number | null;
  isrOff: number | null;
  isrLen: number | null;
  deviceOff: number | null;
  deviceLen: number | null;
};

function parseVirtioPciCaps(cfg: Uint8Array): VirtioPciCaps {
  // Capabilities pointer.
  let ptr = cfg[0x34] ?? 0;
  const caps: VirtioPciCaps = {
    commonOff: null,
    commonLen: null,
    notifyOff: null,
    notifyLen: null,
    notifyMult: null,
    isrOff: null,
    isrLen: null,
    deviceOff: null,
    deviceLen: null,
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
      const length = readU32LE(cfg, ptr + 12);

      switch (cfgType) {
        case VIRTIO_PCI_CAP_COMMON_CFG:
          caps.commonOff = offset;
          caps.commonLen = length;
          break;
        case VIRTIO_PCI_CAP_NOTIFY_CFG:
          caps.notifyOff = offset;
          caps.notifyLen = length;
          if (capLen < 20) throw new Error(`virtio notify cap too short: len=${capLen}`);
          caps.notifyMult = readU32LE(cfg, ptr + 16);
          break;
        case VIRTIO_PCI_CAP_ISR_CFG:
          caps.isrOff = offset;
          caps.isrLen = length;
          break;
        case VIRTIO_PCI_CAP_DEVICE_CFG:
          caps.deviceOff = offset;
          caps.deviceLen = length;
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
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "virtio_net_pci_integration.test" });

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
      expect(pciAddr).toEqual(dev.bdf);

      // Basic PCI identification.
      const idDword = cfgReadU32(pciAddr, 0x00);
      expect(idDword & 0xffff).toBe(0x1af4);
      expect((idDword >>> 16) & 0xffff).toBe(0x1041);
      const subsysDword = cfgReadU32(pciAddr, 0x2c);
      expect(subsysDword & 0xffff).toBe(0x1af4);
      expect((subsysDword >>> 16) & 0xffff).toBe(0x0001);

      // Read BAR0 and ensure it's a 64-bit memory BAR.
      const bar0LowInitial = cfgReadU32(pciAddr, 0x10);
      const bar0HighInitial = cfgReadU32(pciAddr, 0x14);
      // Bits 2:1 = 0b10 indicates a 64-bit memory BAR.
      expect(bar0LowInitial & 0x0f).toBe(0x04);
      expect(bar0HighInitial).toBe(0);
      expect(bar0LowInitial & 0x6).toBe(0x4);
      const oldBar0Base = (BigInt(bar0HighInitial) << 32n) | (BigInt(bar0LowInitial) & 0xffff_fff0n);

      // Read full PCI config space (for capability parsing).
      const cfg = new Uint8Array(256);
      for (let off = 0; off < 256; off += 4) {
        writeU32LE(cfg, off, cfgReadU32(pciAddr, off));
      }

      const caps = parseVirtioPciCaps(cfg);
      // Note: common config is at offset 0x0000 in the Aero virtio-net PCI contract,
      // so we must treat `0` as a valid offset (use `null` as the "not found" sentinel).
      expect(caps.commonOff).not.toBeNull();
      expect(caps.commonLen).not.toBeNull();
      expect(caps.notifyOff).not.toBeNull();
      expect(caps.notifyLen).not.toBeNull();
      expect(caps.isrOff).not.toBeNull();
      expect(caps.isrLen).not.toBeNull();
      expect(caps.deviceOff).not.toBeNull();
      expect(caps.deviceLen).not.toBeNull();
      expect(caps.notifyMult).not.toBeNull();
      expect(caps.notifyMult).toBe(4);

      // Contract v1 virtio-net capability layout (see `io/devices/virtio_net.ts` docs).
      expect(caps.commonOff).toBe(0x0000);
      expect(caps.commonLen).toBe(0x0100);
      expect(caps.notifyOff).toBe(0x1000);
      expect(caps.notifyLen).toBe(0x0100);
      expect(caps.isrOff).toBe(0x2000);
      expect(caps.isrLen).toBe(0x0020);
      expect(caps.deviceOff).toBe(0x3000);
      expect(caps.deviceLen).toBe(0x0100);

      // BAR decoding must be gated on the PCI command register MEM enable bit.
      // Before enabling it, reads should see the unmapped default (all-ones).
      expect(mgr.mmioRead(oldBar0Base + BigInt(caps.commonOff!), 4) >>> 0).toBe(0xffff_ffff);

      // BAR sizing probe (guest writes all-ones then reads back size mask).
      // For BAR0 size=0x4000, mask is 0xFFFF_FFFF_FFFF_C000 (low dword includes type bits 0x4).
      cfgWriteU32(pciAddr, 0x10, 0xffff_ffff);
      cfgWriteU32(pciAddr, 0x14, 0xffff_ffff);
      expect(cfgReadU32(pciAddr, 0x10)).toBe(0xffff_c004);
      expect(cfgReadU32(pciAddr, 0x14)).toBe(0xffff_ffff);

      // Restore original BAR0 assignment after the sizing probe.
      cfgWriteU32(pciAddr, 0x10, bar0LowInitial);
      cfgWriteU32(pciAddr, 0x14, bar0HighInitial);

      // Enable PCI memory decoding (Command register bit 1).
      //
      // Keep Bus Master disabled initially so we can validate that the virtio-net
      // wrapper respects Bus Master Enable when deciding whether to DMA/process
      // virtqueues (mirrors the canonical PC platform behavior).
      // Many guests write the full 32-bit dword at 0x04 with the upper (Status)
      // bits as zero; the TS PCI bus must preserve status bits such as
      // CAP_LIST used by virtio-pci.
      const statusBefore = cfgReadU16(pciAddr, 0x06);
      expect((statusBefore & 0x0010) !== 0).toBe(true);

      const cmd = cfgReadU16(pciAddr, 0x04);
      cfgWriteU32(pciAddr, 0x04, (cmd | 0x2) >>> 0);

      const statusAfter = cfgReadU16(pciAddr, 0x06);
      expect((statusAfter & 0x0010) !== 0).toBe(true);
      const cmdAfter = cfgReadU16(pciAddr, 0x04);
      expect((cmdAfter & 0x0002) !== 0).toBe(true);
      expect((cmdAfter & 0x0004) !== 0).toBe(false);

      // Compute mapped MMIO base.
      const bar0LowBeforeRemap = cfgReadU32(pciAddr, 0x10);
      const bar0HighBeforeRemap = cfgReadU32(pciAddr, 0x14);
      const bar0BaseBeforeRemap =
        (BigInt(bar0HighBeforeRemap) << 32n) | (BigInt(bar0LowBeforeRemap) & 0xffff_fff0n);
      expect(bar0BaseBeforeRemap).toBe(oldBar0Base);

      const commonBaseBeforeRemap = bar0BaseBeforeRemap + BigInt(caps.commonOff!);
      expect(mmioReadU16(commonBaseBeforeRemap + 0x12n)).toBe(2); // num_queues

      // Force BAR0 above 4GiB so we exercise the high dword plumbing, while PCI MEM decoding is enabled.
      const newBarBase = 0x1_0000_0000n; // 4GiB
      const barAttrBits = bar0LowInitial & 0x0f;
      const newBar0Low = ((Number(newBarBase & 0xffff_ffffn) & 0xffff_fff0) | barAttrBits) >>> 0;
      const newBar0High = Number((newBarBase >> 32n) & 0xffff_ffffn) >>> 0;
      cfgWriteU32(pciAddr, 0x10, newBar0Low);
      cfgWriteU32(pciAddr, 0x14, newBar0High);

      const bar0Low = cfgReadU32(pciAddr, 0x10);
      const bar0High = cfgReadU32(pciAddr, 0x14);
      const bar0Base = (BigInt(bar0High) << 32n) | (BigInt(bar0Low) & 0xffff_fff0n);
      expect(bar0Base).toBe(newBarBase);

      const commonBase = bar0Base + BigInt(caps.commonOff!);
      const notifyBase = bar0Base + BigInt(caps.notifyOff!);
      const deviceBase = bar0Base + BigInt(caps.deviceOff!);
      const isrBase = bar0Base + BigInt(caps.isrOff!);

      // Remapping the BAR must unmap the old MMIO window.
      expect(mgr.mmioRead(oldBar0Base + BigInt(caps.commonOff!), 4) >>> 0).toBe(0xffff_ffff);

      // -----------------------------------------------------------------------------------------
      // Virtio modern init (feature negotiation).
      // -----------------------------------------------------------------------------------------
      expect(mmioReadU16(commonBase + 0x12n)).toBe(2); // num_queues
      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE);
      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

      let featuresLo = 0;
      let featuresHi = 0;
      for (const sel of [0, 1]) {
        mmioWriteU32(commonBase + 0x00n, sel);
        const f = mmioReadU32(commonBase + 0x04n);
        if (sel === 0) featuresLo = f;
        else featuresHi = f;
        mmioWriteU32(commonBase + 0x08n, sel);
        mmioWriteU32(commonBase + 0x0cn, f);
      }

      expect((featuresLo & VIRTIO_NET_F_MAC) !== 0).toBe(true);
      expect((featuresLo & VIRTIO_NET_F_STATUS) !== 0).toBe(true);
      expect((featuresLo & VIRTIO_F_RING_INDIRECT_DESC) !== 0).toBe(true);
      expect((featuresHi & VIRTIO_F_VERSION_1_SEL1_BIT) !== 0).toBe(true);
      // Contract v1: no offloads and no mergeable RX buffers.
      expect((featuresLo & VIRTIO_NET_F_CSUM) !== 0).toBe(false);
      expect((featuresLo & VIRTIO_NET_F_MRG_RXBUF) !== 0).toBe(false);

      // Common config readback sanity:
      // - device_feature_select/driver_feature_select should reflect the last written selector (1)
      // - driver_feature should read back the value for the currently selected selector.
      expect(mmioReadU32(commonBase + 0x00n)).toBe(1);
      expect(mmioReadU32(commonBase + 0x08n)).toBe(1);
      expect(mmioReadU32(commonBase + 0x0cn)).toBe(featuresHi);
      mmioWriteU32(commonBase + 0x08n, 0);
      expect(mmioReadU32(commonBase + 0x0cn)).toBe(featuresLo);
      mmioWriteU32(commonBase + 0x08n, 1);

      // Negative feature negotiation coverage: reject invalid feature bits and missing VERSION_1.
      {
        // 1) Invalid (unoffered) feature bit -> FEATURES_OK must be cleared.
        mmioWriteU32(commonBase + 0x08n, 0);
        mmioWriteU32(commonBase + 0x0cn, (featuresLo | VIRTIO_NET_F_CSUM) >>> 0);
        mmioWriteU32(commonBase + 0x08n, 1);
        mmioWriteU32(commonBase + 0x0cn, featuresHi);
        mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);
        const stInvalid = mmioReadU8(commonBase + 0x14n);
        expect(stInvalid & VIRTIO_STATUS_FEATURES_OK).toBe(0);
        expect(stInvalid & (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER)).toBe(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

        // Reset and re-enter driver init.
        mmioWriteU8(commonBase + 0x14n, 0);
        expect(mmioReadU8(commonBase + 0x14n)).toBe(0);
        expect(mmioReadU32(commonBase + 0x00n)).toBe(0);
        expect(mmioReadU32(commonBase + 0x08n)).toBe(0);
        expect(mmioReadU32(commonBase + 0x0cn)).toBe(0);
        expect(mmioReadU16(commonBase + 0x16n)).toBe(0);
        mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE);
        mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

        // 2) Missing VERSION_1 bit for modern transport -> FEATURES_OK must be cleared.
        mmioWriteU32(commonBase + 0x08n, 0);
        mmioWriteU32(commonBase + 0x0cn, featuresLo);
        mmioWriteU32(commonBase + 0x08n, 1);
        mmioWriteU32(commonBase + 0x0cn, (featuresHi & ~VIRTIO_F_VERSION_1_SEL1_BIT) >>> 0);
        mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);
        const stMissingVersion = mmioReadU8(commonBase + 0x14n);
        expect(stMissingVersion & VIRTIO_STATUS_FEATURES_OK).toBe(0);
        expect(stMissingVersion & (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER)).toBe(
          VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
        );

        // Reset and proceed with the correct feature set.
        mmioWriteU8(commonBase + 0x14n, 0);
        expect(mmioReadU8(commonBase + 0x14n)).toBe(0);
        expect(mmioReadU32(commonBase + 0x08n)).toBe(0);
        expect(mmioReadU32(commonBase + 0x0cn)).toBe(0);
        expect(mmioReadU16(commonBase + 0x16n)).toBe(0);
        mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE);
        mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);
        mmioWriteU32(commonBase + 0x08n, 0);
        mmioWriteU32(commonBase + 0x0cn, featuresLo);
        mmioWriteU32(commonBase + 0x08n, 1);
        mmioWriteU32(commonBase + 0x0cn, featuresHi);
      }

      mmioWriteU8(commonBase + 0x14n, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);
      expect(mmioReadU8(commonBase + 0x14n) & VIRTIO_STATUS_FEATURES_OK).not.toBe(0);
      mmioWriteU8(
        commonBase + 0x14n,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK,
      );
      expect(mmioReadU8(commonBase + 0x14n) & VIRTIO_STATUS_DRIVER_OK).not.toBe(0);

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
        expect(mmioReadU16(commonBase + 0x16n)).toBe(queueIndex);

        const max = mmioReadU16(commonBase + 0x18n);
        // Contract v1: virtio-net exposes fixed-size queues (256).
        expect(max).toBe(256);
        // Queue size is treated as read-only by the contract; writes must not change it.
        mmioWriteU16(commonBase + 0x18n, 128);
        expect(mmioReadU16(commonBase + 0x18n)).toBe(256);

        mmioWriteU64(commonBase + 0x20n, BigInt(desc));
        mmioWriteU64(commonBase + 0x28n, BigInt(avail));
        mmioWriteU64(commonBase + 0x30n, BigInt(used));

        const readCommonU64 = (off: bigint): bigint => {
          const lo = BigInt(mmioReadU32(commonBase + off));
          const hi = BigInt(mmioReadU32(commonBase + off + 4n));
          return (hi << 32n) | lo;
        };
        expect(readCommonU64(0x20n)).toBe(BigInt(desc));
        expect(readCommonU64(0x28n)).toBe(BigInt(avail));
        expect(readCommonU64(0x30n)).toBe(BigInt(used));

        const notifyOff = mmioReadU16(commonBase + 0x1en);
        expect(notifyOff).toBe(queueIndex);
        mmioWriteU16(commonBase + 0x1cn, 1);
        expect(mmioReadU16(commonBase + 0x1cn)).toBe(1);
        return notifyOff;
      };

      const rxNotifyOff = configureQueue(0, rxDesc, rxAvail, rxUsed);
      const txNotifyOff = configureQueue(1, txDesc, txAvail, txUsed);
      expect(rxNotifyOff).toBe(0);
      expect(txNotifyOff).toBe(1);

      // -----------------------------------------------------------------------------------------
      // Device config (sanity: ensure MMIO mapping works).
      // -----------------------------------------------------------------------------------------
      const macLo = mmioReadU32(deviceBase);
      const macHi = mmioReadU16(deviceBase + 4n);
      const linkStatus = mmioReadU16(deviceBase + 6n);
      const maxVirtqueuePairs = mmioReadU16(deviceBase + 8n);
      const mac = [
        macLo & 0xff,
        (macLo >>> 8) & 0xff,
        (macLo >>> 16) & 0xff,
        (macLo >>> 24) & 0xff,
        macHi & 0xff,
        (macHi >>> 8) & 0xff,
      ];
      expect(mac.some((b) => b !== 0)).toBe(true);
      // Contract v1: link is always up.
      expect((linkStatus & 1) !== 0).toBe(true);
      expect(maxVirtqueuePairs).toBe(1);

      // Notify region is write-only in virtio-pci; reads should return 0.
      expect(mmioReadU32(notifyBase)).toBe(0);

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

      // With Bus Master disabled, ticking must not service virtqueues / DMA.
      dev.tick(0);
      expect(netTxRing.tryPop()).toBeNull();
      expect(guestReadU16(txUsed + 2)).toBe(0);

      // Enable Bus Master (command bit 2) so the device can DMA and process notified queues.
      cfgWriteU32(pciAddr, 0x04, (cmdAfter | 0x0004) >>> 0);
      expect((cfgReadU16(pciAddr, 0x04) & 0x0004) !== 0).toBe(true);

      let popped: Uint8Array | null = null;
      for (let i = 0; i < 16 && !popped; i++) {
        dev.tick(i);
        popped = netTxRing.tryPop();
      }
      expect(popped).not.toBeNull();
      expect(Array.from(popped!)).toEqual(Array.from(txFrame));
      expect(netTxRing.tryPop()).toBeNull();

      expect(guestReadU16(txUsed + 2)).toBe(1);
      expect(guestReadU32(txUsed + 4)).toBe(0);
      expect(guestReadU32(txUsed + 8)).toBe(0);

      // ISR region should be mapped (not default 0xFF) and read-to-clear.
      const isrAfterTx = mmioReadU8(isrBase);
      expect((isrAfterTx & 0x01) !== 0).toBe(true);
      expect(isrAfterTx & 0xfc).toBe(0);
      expect(mmioReadU8(isrBase)).toBe(0);

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
      expect(guestReadU32(rxUsed + 4)).toBe(0);
      expect(guestReadU32(rxUsed + 8)).toBe(VIRTIO_NET_HDR_LEN + rxFrame.byteLength);

      expect(Array.from(guestReadBytes(rxHdrAddr, VIRTIO_NET_HDR_LEN))).toEqual(Array.from(new Uint8Array(VIRTIO_NET_HDR_LEN)));
      expect(Array.from(guestReadBytes(rxPayloadAddr, rxFrame.byteLength))).toEqual(Array.from(rxFrame));
      expect(netRxRing.tryPop()).toBeNull();

      const isrAfterRx = mmioReadU8(isrBase);
      expect((isrAfterRx & 0x01) !== 0).toBe(true);
      expect(isrAfterRx & 0xfc).toBe(0);
      expect(mmioReadU8(isrBase)).toBe(0);
    } finally {
      try {
        dev.destroy();
      } catch {
        // ignore
      }
    }
  });

  it("TX and RX frames cross NET_TX/NET_RX via virtio-pci transitional legacy I/O transport", async () => {
    // Allocate a wasm memory large enough to host both the Rust/WASM runtime and
    // a small guest RAM window for our virtqueue rings + test buffers.
    const desiredGuestBytes = 0x80_000; // 512 KiB
    const layout = computeGuestRamLayout(desiredGuestBytes);
    const memory = new WebAssembly.Memory({ initial: layout.wasm_pages, maximum: layout.wasm_pages });

    let api: Awaited<ReturnType<typeof initWasm>>["api"];
    try {
      ({ api } = await initWasm({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // The wasm-pack output is generated and may be absent in some test
      // environments; skip rather than failing unrelated suites.
      if (message.includes("Missing single") && message.includes("WASM package")) return;
      throw err;
    }

    assertWasmMemoryWiring({ api, memory, context: "virtio_net_pci_integration.test (legacy)" });

    const Bridge = api.VirtioNetPciBridge;
    if (!Bridge) return;

    const ioIpcSab = createIoIpcSab();
    const netTxRing = openRingByKind(ioIpcSab, IO_IPC_NET_TX_QUEUE_KIND, 0);
    const netRxRing = openRingByKind(ioIpcSab, IO_IPC_NET_RX_QUEUE_KIND, 0);
    netTxRing.reset();
    netRxRing.reset();

    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    // Instantiate a transitional bridge (legacy I/O BAR2 enabled). Older builds may not accept the
    // 4th arg, or may not implement the legacy IO accessors; skip in those cases.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const AnyCtor = Bridge as any;
    let bridge: InstanceType<NonNullable<typeof Bridge>>;
    try {
      bridge = new AnyCtor(layout.guest_base >>> 0, layout.guest_size >>> 0, ioIpcSab, true);
    } catch {
      bridge = new AnyCtor(layout.guest_base >>> 0, layout.guest_size >>> 0, ioIpcSab);
    }
    const bridgeAny = bridge as any;
    const legacyRead =
      typeof bridgeAny.legacy_io_read === "function"
        ? bridgeAny.legacy_io_read
        : typeof bridgeAny.io_read === "function"
          ? bridgeAny.io_read
          : null;
    const legacyWrite =
      typeof bridgeAny.legacy_io_write === "function"
        ? bridgeAny.legacy_io_write
        : typeof bridgeAny.io_write === "function"
          ? bridgeAny.io_write
          : null;

    if (!legacyRead || !legacyWrite) {
      try {
        bridge.free();
      } catch {
        // ignore
      }
      return;
    }
    // Probe HOST_FEATURES: legacy-disabled bridges return all-ones.
    //
    // Note: virtio-pci legacy IO reads are gated by PCI command bit0 (I/O enable). For the probe
    // we temporarily enable I/O decoding inside the bridge so the read is meaningful.
    const setCmd = typeof bridgeAny.set_pci_command === "function" ? bridgeAny.set_pci_command : null;
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
      bridge.free();
      return;
    }

    const dev = new VirtioNetPciDevice({ bridge, irqSink: mgr.irqSink, mode: "transitional" });

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

    // Legacy virtio-pci I/O port register layout (see `crates/aero-virtio/src/pci.rs`).
    const VIRTIO_PCI_LEGACY_HOST_FEATURES = 0x00;
    const VIRTIO_PCI_LEGACY_GUEST_FEATURES = 0x04;
    const VIRTIO_PCI_LEGACY_QUEUE_PFN = 0x08;
    const VIRTIO_PCI_LEGACY_QUEUE_NUM = 0x0c;
    const VIRTIO_PCI_LEGACY_QUEUE_SEL = 0x0e;
    const VIRTIO_PCI_LEGACY_QUEUE_NOTIFY = 0x10;
    const VIRTIO_PCI_LEGACY_STATUS = 0x12;
    const VIRTIO_PCI_LEGACY_ISR = 0x13;

    let pciAddr: PciAddress | null = null;
    try {
      pciAddr = mgr.registerPciDevice(dev);

      const idDword = cfgReadU32(pciAddr, 0x00);
      expect(idDword & 0xffff).toBe(0x1af4);
      expect((idDword >>> 16) & 0xffff).toBe(0x1000);

      // Read BAR2 (legacy IO port block).
      const bar2 = cfgReadU32(pciAddr, 0x18);
      expect(bar2 & 0x1).toBe(0x1);
      const ioBase = bar2 & 0xffff_fffc;
      expect(ioBase).toBeGreaterThan(0);

      // Enable IO + MEM decoding.
      const cmd = cfgReadU16(pciAddr, 0x04);
      cfgWriteU32(pciAddr, 0x04, (cmd | 0x0007) >>> 0);
      const cmdAfter = cfgReadU16(pciAddr, 0x04);
      expect((cmdAfter & 0x0001) !== 0).toBe(true);
      expect((cmdAfter & 0x0002) !== 0).toBe(true);

      const ioReadU8 = (off: number) => mgr.portRead(ioBase + off, 1) & 0xff;
      const ioReadU16 = (off: number) => mgr.portRead(ioBase + off, 2) & 0xffff;
      const ioReadU32 = (off: number) => mgr.portRead(ioBase + off, 4) >>> 0;
      const ioWriteU8 = (off: number, value: number) => mgr.portWrite(ioBase + off, 1, value & 0xff);
      const ioWriteU16 = (off: number, value: number) => mgr.portWrite(ioBase + off, 2, value & 0xffff);
      const ioWriteU32 = (off: number, value: number) => mgr.portWrite(ioBase + off, 4, value >>> 0);

      // -----------------------------------------------------------------------------------------
      // Virtio legacy init (feature negotiation + queue PFN setup).
      // -----------------------------------------------------------------------------------------
      const hostFeatures = ioReadU32(VIRTIO_PCI_LEGACY_HOST_FEATURES);
      expect(hostFeatures).not.toBe(0xffff_ffff);
      expect((hostFeatures & VIRTIO_NET_F_MAC) !== 0).toBe(true);
      expect((hostFeatures & VIRTIO_NET_F_STATUS) !== 0).toBe(true);
      expect((hostFeatures & VIRTIO_F_RING_INDIRECT_DESC) !== 0).toBe(true);
      expect((hostFeatures & VIRTIO_NET_F_CSUM) !== 0).toBe(false);
      expect((hostFeatures & VIRTIO_NET_F_MRG_RXBUF) !== 0).toBe(false);
      ioWriteU32(VIRTIO_PCI_LEGACY_GUEST_FEATURES, hostFeatures);

      // ACKNOWLEDGE | DRIVER.
      ioWriteU8(VIRTIO_PCI_LEGACY_STATUS, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

      const queue0Base = 0x10000;
      const queue1Base = 0x14000;

      const setupQueue = (queueIndex: number, base: number): void => {
        ioWriteU16(VIRTIO_PCI_LEGACY_QUEUE_SEL, queueIndex);
        const max = ioReadU16(VIRTIO_PCI_LEGACY_QUEUE_NUM);
        expect(max).toBe(256);
        ioWriteU32(VIRTIO_PCI_LEGACY_QUEUE_PFN, base >>> 12);
      };
      setupQueue(0, queue0Base);
      setupQueue(1, queue1Base);

      // DRIVER_OK.
      ioWriteU8(
        VIRTIO_PCI_LEGACY_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
      );

      // -----------------------------------------------------------------------------------------
      // TX (queue 1).
      // -----------------------------------------------------------------------------------------
      const txDesc = queue1Base;
      const txAvail = queue1Base + 0x1000;
      const txUsed = queue1Base + 0x2000;

      const hdrAddr = 0x7000;
      const payloadAddr = 0x7100;
      const hdr = new Uint8Array(VIRTIO_NET_HDR_LEN); // all zeros
      const txFrame = new Uint8Array([0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0x08, 0x00]);
      guestWriteBytes(hdrAddr, hdr);
      guestWriteBytes(payloadAddr, txFrame);

      guestWriteDesc(txDesc, 0, hdrAddr, hdr.byteLength, VIRTQ_DESC_F_NEXT, 1);
      guestWriteDesc(txDesc, 1, payloadAddr, txFrame.byteLength, 0, 0);

      guestWriteU16(txAvail + 0, 0);
      guestWriteU16(txAvail + 2, 1);
      guestWriteU16(txAvail + 4, 0);
      guestWriteU16(txUsed + 0, 0);
      guestWriteU16(txUsed + 2, 0);
      guestWriteU32(txUsed + 4, 0);
      guestWriteU32(txUsed + 8, 0);

      ioWriteU16(VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, 1);
      dev.tick(0);

      let popped: Uint8Array | null = null;
      for (let i = 0; i < 16 && !popped; i++) {
        dev.tick(i);
        popped = netTxRing.tryPop();
      }
      expect(popped).not.toBeNull();
      expect(Array.from(popped!)).toEqual(Array.from(txFrame));
      expect(netTxRing.tryPop()).toBeNull();

      expect(guestReadU16(txUsed + 2)).toBe(1);
      expect(guestReadU32(txUsed + 4)).toBe(0);
      expect(guestReadU32(txUsed + 8)).toBe(0);

      const isrAfterTx = ioReadU8(VIRTIO_PCI_LEGACY_ISR);
      expect((isrAfterTx & 0x01) !== 0).toBe(true);
      expect(isrAfterTx & 0xfc).toBe(0);
      expect(ioReadU8(VIRTIO_PCI_LEGACY_ISR)).toBe(0);

      // -----------------------------------------------------------------------------------------
      // RX (queue 0).
      // -----------------------------------------------------------------------------------------
      const rxDesc = queue0Base;
      const rxAvail = queue0Base + 0x1000;
      const rxUsed = queue0Base + 0x2000;

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

      ioWriteU16(VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, 0);

      for (let i = 0; i < 8 && guestReadU16(rxUsed + 2) !== 1; i++) {
        dev.tick(i);
      }

      expect(guestReadU16(rxUsed + 2)).toBe(1);
      expect(guestReadU32(rxUsed + 4)).toBe(0);
      expect(guestReadU32(rxUsed + 8)).toBe(VIRTIO_NET_HDR_LEN + rxFrame.byteLength);

      expect(Array.from(guestReadBytes(rxHdrAddr, VIRTIO_NET_HDR_LEN))).toEqual(Array.from(new Uint8Array(VIRTIO_NET_HDR_LEN)));
      expect(Array.from(guestReadBytes(rxPayloadAddr, rxFrame.byteLength))).toEqual(Array.from(rxFrame));
      expect(netRxRing.tryPop()).toBeNull();

      const isrAfterRx = ioReadU8(VIRTIO_PCI_LEGACY_ISR);
      expect((isrAfterRx & 0x01) !== 0).toBe(true);
      expect(isrAfterRx & 0xfc).toBe(0);
      expect(ioReadU8(VIRTIO_PCI_LEGACY_ISR)).toBe(0);
    } finally {
      try {
        dev.destroy();
      } catch {
        // ignore
      }
    }
  });
});
