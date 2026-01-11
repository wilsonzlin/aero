import { parentPort, workerData } from "node:worker_threads";

import { RingBuffer } from "../../src/ipc/ring_buffer.ts";
import { AeroIpcIoClient } from "../../src/io/ipc/aero_ipc_io.ts";

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

const io = new AeroIpcIoClient(cmdQ, evtQ);

try {
  const guestReadOffset = 32;
  const readLen = 16;
  const readResp = io.diskRead(0n, readLen, BigInt(guestReadOffset), 2000);
  const guestBytes = Array.from(guest.subarray(guestReadOffset, guestReadOffset + readLen));

  const guestWriteOffset = 128;
  guest.set([0xde, 0xad, 0xbe, 0xef], guestWriteOffset);
  const writeResp = io.diskWrite(300n, 4, BigInt(guestWriteOffset), 2000);

  const diskBytesAfterWrite = Array.from(disk.subarray(300, 304));

  parentPort!.postMessage({
    ok: true,
    readResp: { ok: readResp.ok, bytes: readResp.bytes, errorCode: readResp.errorCode },
    writeResp: { ok: writeResp.ok, bytes: writeResp.bytes, errorCode: writeResp.errorCode },
    guestBytes,
    diskBytesAfterWrite,
  });
} catch (err) {
  parentPort!.postMessage({ ok: false, error: err instanceof Error ? err.message : String(err) });
}

