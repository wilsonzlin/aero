import { PROTOCOL, RingBufferI32 } from "./memory-model.js";

function wlog(line) {
  self.postMessage({ type: "log", line });
}

/** @type {WebAssembly.Memory | null} */
let guestMemory = null;
/** @type {RingBufferI32 | null} */
let cmdRing = null;
/** @type {RingBufferI32 | null} */
let eventRing = null;

/** @type {Int32Array | null} */
let guestI32 = null;

self.onmessage = (ev) => {
  const msg = ev.data;
  if (msg?.type !== "init") return;

  guestMemory = msg.guestMemory;
  cmdRing = new RingBufferI32(new Int32Array(msg.cmdSab));
  eventRing = new RingBufferI32(new Int32Array(msg.eventSab));
  guestI32 = new Int32Array(guestMemory.buffer);

  wlog("init: received shared guestMemory + cmdSab + eventSab");
  wlog(`init: guestMemory bytes = ${guestMemory.buffer.byteLength}`);

  mainLoop();
};

function mainLoop() {
  if (!cmdRing || !eventRing || !guestI32) return;

  wlog("ready: waiting for commands via Atomics.wait()");

  // eslint-disable-next-line no-constant-condition
  while (true) {
    let msg = cmdRing.popMessage();
    if (!msg) {
      cmdRing.waitForDataBlocking();
      msg = cmdRing.popMessage();
      if (!msg) continue;
    }

    const [opcode, a0] = msg;
    if (opcode === PROTOCOL.OP_INC32_AT_OFFSET) {
      const byteOffset = a0 >>> 0;
      const index = byteOffset >>> 2;
      const before = Atomics.add(guestI32, index, 1);
      const after = before + 1;

      const ok = eventRing.pushMessage([opcode, after | 0, 0, 0]);
      if (!ok) {
        wlog("event: ring full; dropping response");
        continue;
      }
      eventRing.notifyData();
      wlog(`cmd: Atomics.add(guestI32[${index}]) @ ${byteOffset} => ${before} -> ${after}`);
      continue;
    }

    if (!eventRing.pushMessage([opcode, 0, 1, 0])) {
      wlog("event: ring full; dropping error response");
      continue;
    }
    eventRing.notifyData();
    wlog(`cmd: unknown opcode ${opcode}`);
  }
}
