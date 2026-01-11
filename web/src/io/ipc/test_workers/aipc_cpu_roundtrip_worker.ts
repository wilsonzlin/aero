import { parentPort, workerData } from "node:worker_threads";

import { openRingByKind } from "../../../ipc/ipc.ts";
import { queueKind } from "../../../ipc/layout.ts";
import { AeroIpcIoClient } from "../aero_ipc_io.ts";

const { ipcBuffer } = workerData as { ipcBuffer: SharedArrayBuffer };

const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

const io = new AeroIpcIoClient(cmdQ, evtQ);

try {
  const status64 = io.portRead(0x64, 1);
  io.portWrite(0x64, 1, 0x20);
  const cmdByte = io.portRead(0x60, 1);

  parentPort!.postMessage({
    ok: true,
    status64,
    cmdByte,
  });
} catch (err) {
  parentPort!.postMessage({ ok: false, error: err instanceof Error ? err.message : String(err) });
}

