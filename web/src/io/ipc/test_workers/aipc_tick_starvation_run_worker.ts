import { parentPort, workerData } from "node:worker_threads";

import { openRingByKind, parseIpcBuffer } from "../../../ipc/ipc.ts";
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

// Introspect the ring control header so the test can assert that tick() fired while
// commands were still queued (i.e. not only when the ring became empty).
const { queues } = parseIpcBuffer(ipcBuffer);
const cmdOffset = queues.find((q) => q.kind === queueKind.CMD)?.offsetBytes;
if (cmdOffset == null) throw new Error("CMD ring not found in IPC buffer");
const cmdCtrl = new Int32Array(ipcBuffer, cmdOffset, ringCtrl.WORDS);

const tickCounterI32 = new Int32Array(tickCounter);
const tickSawCmdDataI32 = new Int32Array(tickSawCmdData);

let busyAcc = 0;

const target: AeroIpcIoDispatchTarget = {
  portRead: () => 0,
  portWrite: () => {
    // Add deterministic work so draining takes long enough that ticks would
    // starve without the opportunistic tick-in-drain behavior.
    for (let i = 0; i < (workIters >>> 0); i++) busyAcc = Math.imul(busyAcc + i, 0x9e37_79b1);
    // Store the accumulator into shared memory so this work remains observable (and cannot be
    // optimized away as a pure computation).
    if (tickSawCmdDataI32.length > 1) Atomics.store(tickSawCmdDataI32, 1, busyAcc | 0);
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
