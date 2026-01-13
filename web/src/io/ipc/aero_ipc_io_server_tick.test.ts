import { describe, expect, it } from "vitest";

import { ringCtrl } from "../../ipc/layout";
import { encodeCommand } from "../../ipc/protocol";
import { RingBuffer } from "../../ipc/ring_buffer";

import { AeroIpcIoServer, type AeroIpcIoDispatchTarget } from "./aero_ipc_io";

function makeRing(capacityBytes: number): RingBuffer {
  const cap = Math.max(0, Math.floor(capacityBytes)) >>> 0;
  const sab = new SharedArrayBuffer(ringCtrl.BYTES + cap);
  new Int32Array(sab, 0, ringCtrl.WORDS).set([0, 0, 0, cap]);
  return new RingBuffer(sab, 0);
}

describe("io/ipc/aero_ipc_io:AeroIpcIoServer.runAsync", () => {
  it("ticks opportunistically while draining commands under sustained load", async () => {
    const cmdQ = makeRing(4096);
    const evtQ = makeRing(4096);

    let tickCount = 0;
    const target: AeroIpcIoDispatchTarget = {
      portRead: () => 0,
      portWrite: () => {},
      mmioRead: () => 0,
      mmioWrite: () => {},
      tick: () => {
        tickCount++;
      },
    };

    const server = new AeroIpcIoServer(cmdQ, evtQ, target, {
      tickIntervalMs: 1,
      // Drop events so the event ring cannot fill and block the server.
      emitEvent: () => {},
    });

    const nop = encodeCommand({ kind: "nop", seq: 0 });
    while (cmdQ.tryPush(nop)) {
      // Prefill the queue so it never becomes empty even if the producer is delayed.
    }

    const controller = new AbortController();
    const serverTask = server.runAsync({
      signal: controller.signal,
      yieldEveryNCommands: 8,
    });

    const start = performance.now();
    while (performance.now() - start < 50) {
      // Keep the command queue full so `runAsync` stays in its drain loop.
      while (cmdQ.tryPush(nop)) {}
      // Wait until the server consumes at least one record, then fill again.
      await cmdQ.waitForConsumeAsync(10);
    }

    controller.abort();
    await serverTask;

    expect(tickCount).toBeGreaterThan(5);
  });
});

