import { createIpcBuffer } from "../../ipc/ipc.ts";
import { queueKind } from "../../ipc/layout.ts";
import { RingBuffer } from "../../ipc/ring_buffer.ts";

export interface IoIpcChannel {
  buffer: SharedArrayBuffer;
  cmd: RingBuffer;
  evt: RingBuffer;
}

export interface IoIpcChannelOptions {
  cmdCapacityBytes?: number;
  evtCapacityBytes?: number;
}

/**
 * Helper to allocate a 2-queue AIPC buffer suitable for CPU↔I/O device calls.
 *
 * The CPU side should treat `cmd` as CPU→IO and `evt` as IO→CPU.
 *
 * The IO worker should open the same rings and treat them in the opposite
 * direction.
 */
export function createIoIpcChannel(opts: IoIpcChannelOptions = {}): IoIpcChannel {
  const cmdCapacityBytes = opts.cmdCapacityBytes ?? 1 << 16;
  const evtCapacityBytes = opts.evtCapacityBytes ?? 1 << 16;

  const { buffer, queues } = createIpcBuffer([
    { kind: queueKind.CMD, capacityBytes: cmdCapacityBytes },
    { kind: queueKind.EVT, capacityBytes: evtCapacityBytes },
  ]);

  const cmdInfo = queues.find((q) => q.kind === queueKind.CMD);
  const evtInfo = queues.find((q) => q.kind === queueKind.EVT);
  if (!cmdInfo || !evtInfo) {
    throw new Error("createIpcBuffer did not return expected CMD/EVT queue descriptors");
  }

  const cmd = new RingBuffer(buffer, cmdInfo.offsetBytes);
  const evt = new RingBuffer(buffer, evtInfo.offsetBytes);
  return { buffer, cmd, evt };
}

