import { workerData } from "node:worker_threads";

import { RingBuffer } from "../../src/ipc/ring_buffer.ts";
import { AeroIpcIoServer, type AeroIpcIoDiskResult, type AeroIpcIoDispatchTarget } from "../../src/io/ipc/aero_ipc_io.ts";

const { sab, cmdOffset, evtOffset, guestSab, diskSab } = workerData as {
  sab: SharedArrayBuffer;
  cmdOffset: number;
  evtOffset: number;
  guestSab: SharedArrayBuffer;
  diskSab: SharedArrayBuffer;
};

const cmdQ = new RingBuffer(sab, cmdOffset);
const evtQ = new RingBuffer(sab, evtOffset);

const guest = new Uint8Array(guestSab);
const disk = new Uint8Array(diskSab);

function boundsCheck(offset: number, len: number, capacity: number): boolean {
  if (!Number.isFinite(offset) || offset < 0) return false;
  if (!Number.isFinite(len) || len < 0) return false;
  if (offset + len > capacity) return false;
  return true;
}

const target: AeroIpcIoDispatchTarget = {
  portRead: () => 0,
  portWrite: () => {},
  mmioRead: () => 0,
  mmioWrite: () => {},
  tick: () => {},

  diskRead: (diskOffset, len, guestOffset): AeroIpcIoDiskResult => {
    const length = len >>> 0;
    const diskOff = Number(diskOffset);
    const guestOff = Number(guestOffset);
    if (!Number.isSafeInteger(diskOff) || !Number.isSafeInteger(guestOff)) {
      return { ok: false, bytes: 0, errorCode: 1 };
    }
    if (!boundsCheck(diskOff, length, disk.byteLength) || !boundsCheck(guestOff, length, guest.byteLength)) {
      return { ok: false, bytes: 0, errorCode: 2 };
    }
    guest.set(disk.subarray(diskOff, diskOff + length), guestOff);
    return { ok: true, bytes: length };
  },

  diskWrite: (diskOffset, len, guestOffset): AeroIpcIoDiskResult => {
    const length = len >>> 0;
    const diskOff = Number(diskOffset);
    const guestOff = Number(guestOffset);
    if (!Number.isSafeInteger(diskOff) || !Number.isSafeInteger(guestOff)) {
      return { ok: false, bytes: 0, errorCode: 1 };
    }
    if (!boundsCheck(diskOff, length, disk.byteLength) || !boundsCheck(guestOff, length, guest.byteLength)) {
      return { ok: false, bytes: 0, errorCode: 2 };
    }
    disk.set(guest.subarray(guestOff, guestOff + length), diskOff);
    return { ok: true, bytes: length };
  },
};

new AeroIpcIoServer(cmdQ, evtQ, target, { tickIntervalMs: 1 }).run();

