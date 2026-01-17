import { parentPort, workerData } from "node:worker_threads";

import { openRingByKind } from "../../../ipc/ipc.ts";
import { queueKind } from "../../../ipc/layout.ts";
import { VRAM_BASE_PADDR } from "../../../arch/guest_phys.ts";
import { formatOneLineError } from "../../../text.ts";
import { AeroIpcIoClient } from "../aero_ipc_io.ts";

const { ipcBuffer, vram } = workerData as { ipcBuffer: SharedArrayBuffer; vram?: SharedArrayBuffer };

const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

const irqEvents: Array<{ irq: number; level: boolean }> = [];
const a20Events: boolean[] = [];
let resetRequests = 0;
const serialBytes: number[] = [];

const io = new AeroIpcIoClient(cmdQ, evtQ, {
  onIrq: (irq, level) => irqEvents.push({ irq, level }),
  onA20: (enabled) => a20Events.push(enabled),
  onReset: () => {
    resetRequests += 1;
  },
  onSerialOutput: (_port, data) => {
    for (const b of data) serialBytes.push(b);
  },
});

try {
  const status64 = io.portRead(0x64, 1);
  io.portWrite(0x64, 1, 0x20);
  const cmdByte = io.portRead(0x60, 1);

  // Enable IRQ1 for keyboard output, then send a keyboard reset command (0xFF).
  io.portWrite(0x64, 1, 0x60);
  io.portWrite(0x60, 1, 0x01);
  io.portWrite(0x60, 1, 0xff);
  const kbd0 = io.portRead(0x60, 1);
  const kbd1 = io.portRead(0x60, 1);

  // Toggle A20 via i8042 output port (0xD1).
  io.portWrite(0x64, 1, 0xd1);
  io.portWrite(0x60, 1, 0x03);

  // Reset request via i8042 command (0xFE).
  io.portWrite(0x64, 1, 0xfe);

  // Emit a couple bytes via COM1 THR.
  io.portWrite(0x3f8, 1, 0x48);
  io.portWrite(0x3f8, 1, 0x69);

  // MMIO test device (0x1000_0000) roundtrip.
  io.mmioWrite(0x1000_0000n, 4, 0x1234_5678);
  const mmio0 = io.mmioRead(0x1000_0000n, 4);

  // PCI config + BAR mapping test (PciTestDevice, bus0/dev0/fn0).
  const pciAddrBase = 0x8000_0000; // enable bit
  const pciCfgAddr = (reg: number) => (pciAddrBase | (reg & 0xfc)) >>> 0;
  const pciReadDword = (reg: number) => {
    io.portWrite(0x0cf8, 4, pciCfgAddr(reg));
    return io.portRead(0x0cfc, 4) >>> 0;
  };
  const pciWriteDword = (reg: number, value: number) => {
    io.portWrite(0x0cf8, 4, pciCfgAddr(reg));
    io.portWrite(0x0cfc, 4, value >>> 0);
  };

  const pciId = pciReadDword(0x00);
  const pciVendorId = pciId & 0xffff;
  const pciDeviceId = (pciId >>> 16) & 0xffff;
  const pciBar0 = pciReadDword(0x10);

  // Enable memory-space decoding (command bit1).
  pciWriteDword(0x04, 0x0000_0002);
  const pciBar0Base = BigInt(pciBar0 >>> 0) & 0xffff_fff0n;
  io.mmioWrite(pciBar0Base, 4, 0xcafe_babe);
  const pciMmio0 = io.mmioRead(pciBar0Base, 4);

  // VRAM-backed MMIO range (BAR1-style aperture) integration test.
  let vramMmio = 0;
  let vramBytes: number[] | null = null;
  if (vram instanceof SharedArrayBuffer) {
    const vramU8 = new Uint8Array(vram);
    const base = BigInt(VRAM_BASE_PADDR >>> 0);
    io.mmioWrite(base + 0x10n, 4, 0xdead_beef);
    vramMmio = io.mmioRead(base + 0x10n, 4) >>> 0;
    vramBytes = Array.from(vramU8.subarray(0x10, 0x14));
  }

  parentPort!.postMessage({
    ok: true,
    status64,
    cmdByte,
    kbd: [kbd0, kbd1],
    irqEvents,
    a20Events,
    resetRequests,
    serialBytes,
    mmio0,
    pciVendorId,
    pciDeviceId,
    pciBar0,
    pciMmio0,
    vramMmio,
    vramBytes,
  });
} catch (err) {
  parentPort!.postMessage({ ok: false, error: formatOneLineError(err, 512) });
}
