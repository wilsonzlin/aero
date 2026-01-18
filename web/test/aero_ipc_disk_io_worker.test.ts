import test from "node:test";
import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";
import { once } from "node:events";

import { alignUp, ringCtrl } from "../src/ipc/layout.ts";
import { WORKER_EXEC_ARGV } from "./_helpers/worker_exec_argv.ts";

function createCmdEvtSharedBuffer(
  cmdCapBytes: number,
  evtCapBytes: number,
): { sab: SharedArrayBuffer; cmdOffset: number; evtOffset: number } {
  const cmdOffset = 0;
  const evtOffset = alignUp(cmdOffset + ringCtrl.BYTES + cmdCapBytes, 4);
  const totalBytes = evtOffset + ringCtrl.BYTES + evtCapBytes;

  const sab = new SharedArrayBuffer(totalBytes);
  new Int32Array(sab, cmdOffset, ringCtrl.WORDS).set([0, 0, 0, cmdCapBytes]);
  new Int32Array(sab, evtOffset, ringCtrl.WORDS).set([0, 0, 0, evtCapBytes]);

  return { sab, cmdOffset, evtOffset };
}

test("AIPC I/O server: diskRead/diskWrite copy between shared guest memory + disk buffer", async () => {
  const { sab, cmdOffset, evtOffset } = createCmdEvtSharedBuffer(1 << 16, 1 << 16);

  const guestSab = new SharedArrayBuffer(4096);
  const diskSab = new SharedArrayBuffer(4096);

  // Seed the "disk" with a known pattern.
  const disk = new Uint8Array(diskSab);
  for (let i = 0; i < 256; i++) disk[i] = i & 0xff;

  const ioWorker = new Worker(new URL("./workers/aero_ipc_disk_server_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, guestSab, diskSab },
    execArgv: WORKER_EXEC_ARGV,
  });

  const cpuWorker = new Worker(new URL("./workers/aero_ipc_cpu_disk_worker.ts", import.meta.url), {
    type: "module",
    workerData: { sab, cmdOffset, evtOffset, guestSab, diskSab },
    execArgv: WORKER_EXEC_ARGV,
  });

  try {
    const [result] = (await once(cpuWorker, "message")) as [
      {
        ok: boolean;
        readResp: { ok: boolean; bytes: number; errorCode?: number };
        writeResp: { ok: boolean; bytes: number; errorCode?: number };
        guestBytes: number[];
        diskBytesAfterWrite: number[];
      },
    ];

    assert.equal(result.ok, true);
    assert.equal(result.readResp.ok, true);
    assert.equal(result.readResp.bytes, 16);
    assert.deepEqual(result.guestBytes, Array.from({ length: 16 }, (_, i) => i & 0xff));

    assert.equal(result.writeResp.ok, true);
    assert.equal(result.writeResp.bytes, 4);
    assert.deepEqual(result.diskBytesAfterWrite, [0xde, 0xad, 0xbe, 0xef]);
  } finally {
    cpuWorker.unref();
    ioWorker.unref();
    const done = Promise.allSettled([cpuWorker.terminate(), ioWorker.terminate()]);
    await Promise.race([done, new Promise<void>((resolve) => setTimeout(resolve, 2000))]);
  }
});

