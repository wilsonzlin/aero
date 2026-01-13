import { parentPort, workerData } from "node:worker_threads";

import { openRingByKind } from "../../../ipc/ipc.ts";
import { queueKind, ringCtrl } from "../../../ipc/layout.ts";
import { AeroIpcIoServer, type AeroIpcIoDispatchTarget } from "../aero_ipc_io.ts";

const { ipcBuffer, tickCounter, tickSawCmdData, tickIntervalMs, workIters } = workerData as {
  ipcBuffer: SharedArrayBuffer;
  tickCounter: SharedArrayBuffer;
  tickSawCmdData: SharedArrayBuffer;
  tickIntervalMs: number;
  workIters: number;
};

const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

// Access RingBuffer's private header view. This is safe in tests and lets us verify
// whether tick() fired while the command ring still had pending data.
const cmdCtrl = (cmdQ as unknown as { ctrl: Int32Array }).ctrl;

const tickCounterI32 = new Int32Array(tickCounter);
const tickSawCmdDataI32 = new Int32Array(tickSawCmdData);

let busyAcc = 0;

const target: AeroIpcIoDispatchTarget = {
  portRead: () => 0,
  portWrite: () => {
    // Add deterministic work so draining takes long enough that ticks would
    // starve without the opportunistic tick-in-drain behavior.
    for (let i = 0; i < (workIters >>> 0); i++) busyAcc = (busyAcc + i) | 0;
  },
  mmioRead: () => 0,
  mmioWrite: () => {},
  tick: () => {
    Atomics.add(tickCounterI32, 0, 1);
    const head = Atomics.load(cmdCtrl, ringCtrl.HEAD);
    const tail = Atomics.load(cmdCtrl, ringCtrl.TAIL_COMMIT);
    if (head !== tail) Atomics.store(tickSawCmdDataI32, 0, 1);
  },
};

parentPort!.postMessage({ type: "ready" });

new AeroIpcIoServer(cmdQ, evtQ, target, {
  tickIntervalMs,
  // Drop responses/acks; the tests don't consume them and we don't want to stall
  // on a full event ring.
  emitEvent: () => {},
}).run();

