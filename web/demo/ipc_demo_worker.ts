/// <reference lib="webworker" />

import { decodeCommand, encodeEvent } from "../src/ipc/protocol";
import { RingBuffer } from "../src/ipc/ring_buffer";

type InitMessage = {
  sab: SharedArrayBuffer;
  cmdOffset: number;
  evtOffset: number;
};

self.onmessage = (ev: MessageEvent<InitMessage>) => {
  const { sab, cmdOffset, evtOffset } = ev.data;
  const cmdQ = new RingBuffer(sab, cmdOffset);
  const evtQ = new RingBuffer(sab, evtOffset);

  // Main loop: block for data, respond with ack.
  for (;;) {
    const msg = cmdQ.popBlocking();
    const cmd = decodeCommand(msg);
    switch (cmd.kind) {
      case "nop":
        evtQ.pushBlocking(encodeEvent({ kind: "ack", seq: cmd.seq }));
        break;
      case "shutdown":
        return;
      default:
        // Ignore.
        break;
    }
  }
};

