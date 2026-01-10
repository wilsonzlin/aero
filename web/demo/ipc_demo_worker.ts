/// <reference lib="webworker" />

import { decodeCommand, encodeEvent } from "../src/ipc/protocol";
import { queueKind } from "../src/ipc/layout";
import { openRingByKind } from "../src/ipc/ipc";

type InitMessage = {
  sab: SharedArrayBuffer;
};

self.onmessage = (ev: MessageEvent<InitMessage>) => {
  const { sab } = ev.data;
  const cmdQ = openRingByKind(sab, queueKind.CMD);
  const evtQ = openRingByKind(sab, queueKind.EVT);

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
