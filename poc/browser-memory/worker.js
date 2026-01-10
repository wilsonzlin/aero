import { CMD } from "./memory-model.js";

function wlog(line) {
  self.postMessage({ type: "log", line });
}

/** @type {WebAssembly.Memory | null} */
let guestMemory = null;
/** @type {Int32Array | null} */
let cmdI32 = null;

/** @type {Uint32Array | null} */
let guestU32 = null;

self.onmessage = (ev) => {
  const msg = ev.data;
  if (msg?.type !== "init") return;

  guestMemory = msg.guestMemory;
  cmdI32 = new Int32Array(msg.cmdSab);
  guestU32 = new Uint32Array(guestMemory.buffer);

  wlog("init: received shared guestMemory + cmdSab");
  wlog(`init: guestMemory bytes = ${guestMemory.buffer.byteLength}`);

  mainLoop();
};

function mainLoop() {
  if (!cmdI32 || !guestU32) return;

  wlog("ready: waiting for commands via Atomics.wait()");

  // eslint-disable-next-line no-constant-condition
  while (true) {
    // Wait while IDLE. When main flips to REQUEST and notifies, this returns.
    Atomics.wait(cmdI32, CMD.I_STATE, CMD.STATE_IDLE);

    const state = Atomics.load(cmdI32, CMD.I_STATE);
    if (state !== CMD.STATE_REQUEST) {
      // Spurious wake or main reset; continue.
      continue;
    }

    const opcode = Atomics.load(cmdI32, CMD.I_OPCODE);
    if (opcode === CMD.OP_INC32_AT_OFFSET) {
      const byteOffset = Atomics.load(cmdI32, CMD.I_ARG0) >>> 0;
      const index = byteOffset >>> 2;
      const before = guestU32[index] >>> 0;
      const after = (before + 1) >>> 0;
      guestU32[index] = after;

      Atomics.store(cmdI32, CMD.I_RESULT0, after);
      Atomics.store(cmdI32, CMD.I_ERROR, 0);
      Atomics.store(cmdI32, CMD.I_STATE, CMD.STATE_RESPONSE);
      Atomics.notify(cmdI32, CMD.I_STATE, 1);
      wlog(`cmd: INC32 @ ${byteOffset} => ${before} -> ${after}`);
      continue;
    }

    Atomics.store(cmdI32, CMD.I_ERROR, 1);
    Atomics.store(cmdI32, CMD.I_STATE, CMD.STATE_RESPONSE);
    Atomics.notify(cmdI32, CMD.I_STATE, 1);
    wlog(`cmd: unknown opcode ${opcode}`);
  }
}

