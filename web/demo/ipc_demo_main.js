// Minimal browser demo for the Aero IPC ring buffers.
//
// This intentionally avoids any bundler so it can be opened from a simple dev
// server. The production TS modules live in `web/src/ipc/*`.

const RECORD_ALIGN = 4;
const WRAP_MARKER = 0xffff_ffff;

const ringCtrl = { HEAD: 0, TAIL_RESERVE: 1, TAIL_COMMIT: 2, CAPACITY: 3, WORDS: 4, BYTES: 16 };

function alignUp(value, align) {
  return (value + (align - 1)) & ~(align - 1);
}

function u32(n) {
  return n >>> 0;
}

class RingBuffer {
  constructor(buffer, offsetBytes) {
    this.ctrl = new Int32Array(buffer, offsetBytes, ringCtrl.WORDS);
    this.cap = u32(Atomics.load(this.ctrl, ringCtrl.CAPACITY));
    this.data = new Uint8Array(buffer, offsetBytes + ringCtrl.BYTES, this.cap);
    this.view = new DataView(this.data.buffer, this.data.byteOffset, this.data.byteLength);
  }

  tryPush(payload) {
    const payloadLen = payload.byteLength;
    const recordSize = alignUp(4 + payloadLen, RECORD_ALIGN);
    if (recordSize > this.cap) return false;

    for (;;) {
      const head = u32(Atomics.load(this.ctrl, ringCtrl.HEAD));
      const tail = u32(Atomics.load(this.ctrl, ringCtrl.TAIL_RESERVE));

      const used = u32(tail - head);
      if (used > this.cap) continue;
      const free = this.cap - used;

      const tailIndex = tail % this.cap;
      const remaining = this.cap - tailIndex;
      const needsWrap = remaining >= 4 && remaining < recordSize;
      const padding = remaining < recordSize ? remaining : 0;
      const reserve = padding + recordSize;
      if (reserve > free) return false;

      const newTail = u32(tail + reserve);
      const prev = Atomics.compareExchange(this.ctrl, ringCtrl.TAIL_RESERVE, tail | 0, newTail | 0);
      if (u32(prev) !== tail) continue;

      if (needsWrap) this.view.setUint32(tailIndex, WRAP_MARKER, true);

      const start = u32(tail + padding);
      const startIndex = start % this.cap;
      this.view.setUint32(startIndex, payloadLen, true);
      this.data.set(payload, startIndex + 4);

      // Commit in-order.
      while (u32(Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT)) !== tail) {
        // main thread can't Atomics.wait; spin.
      }
      Atomics.store(this.ctrl, ringCtrl.TAIL_COMMIT, newTail | 0);
      Atomics.notify(this.ctrl, ringCtrl.TAIL_COMMIT, 1);
      return true;
    }
  }

  tryPop() {
    for (;;) {
      const head = u32(Atomics.load(this.ctrl, ringCtrl.HEAD));
      const tail = u32(Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT));
      if (head === tail) return null;

      const headIndex = head % this.cap;
      const remaining = this.cap - headIndex;
      if (remaining < 4) {
        Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + remaining) | 0);
        Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
        continue;
      }

      const len = this.view.getUint32(headIndex, true);
      if (len === WRAP_MARKER) {
        Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + remaining) | 0);
        Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
        continue;
      }
      const total = alignUp(4 + len, RECORD_ALIGN);
      if (total > remaining) return null;

      const start = headIndex + 4;
      const out = this.data.slice(start, start + len);
      Atomics.store(this.ctrl, ringCtrl.HEAD, u32(head + total) | 0);
      Atomics.notify(this.ctrl, ringCtrl.HEAD, 1);
      return out;
    }
  }
}

function encodeNop(seq) {
  const out = new Uint8Array(2 + 4);
  const view = new DataView(out.buffer);
  view.setUint16(0, 0x0000, true);
  view.setUint32(2, seq >>> 0, true);
  return out;
}

function decodeAck(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const tag = view.getUint16(0, true);
  if (tag !== 0x1000) throw new Error("not ack");
  return view.getUint32(2, true);
}

const startBtn = document.getElementById("start");
const stopBtn = document.getElementById("stop");
const statsEl = document.getElementById("stats");

let running = false;

startBtn.onclick = async () => {
  if (typeof SharedArrayBuffer === "undefined") {
    alert("SharedArrayBuffer unavailable. Ensure COOP/COEP headers are set.");
    return;
  }

  startBtn.disabled = true;
  stopBtn.disabled = false;
  running = true;

  const CMD_CAP = 1 << 20; // 1 MiB
  const EVT_CAP = 1 << 20;
  const cmdOffset = 0;
  const evtOffset = alignUp(cmdOffset + ringCtrl.BYTES + CMD_CAP, 4);
  const totalBytes = evtOffset + ringCtrl.BYTES + EVT_CAP;

  const sab = new SharedArrayBuffer(totalBytes);

  // Init ring headers (head/tail reserved/commit = 0; capacity set).
  new Int32Array(sab, cmdOffset, ringCtrl.WORDS).set([0, 0, 0, CMD_CAP]);
  new Int32Array(sab, evtOffset, ringCtrl.WORDS).set([0, 0, 0, EVT_CAP]);

  const cmdQ = new RingBuffer(sab, cmdOffset);
  const evtQ = new RingBuffer(sab, evtOffset);

  const worker = new Worker("./ipc_demo_worker.js", { type: "module" });
  worker.postMessage({ sab, cmdOffset, evtOffset });

  let sent = 0;
  let received = 0;
  let lastSeq = 0;
  let startTime = performance.now();
  let lastUpdate = startTime;

  function pump() {
    if (!running) return;

    // Send as much as we can each frame.
    for (let i = 0; i < 50_000; i++) {
      if (!cmdQ.tryPush(encodeNop(sent))) break;
      sent++;
    }

    // Drain events.
    for (;;) {
      const msg = evtQ.tryPop();
      if (!msg) break;
      lastSeq = decodeAck(msg);
      received++;
    }

    const now = performance.now();
    if (now - lastUpdate > 200) {
      const elapsed = (now - startTime) / 1000;
      statsEl.textContent =
        `sent:     ${sent}\n` +
        `received: ${received}\n` +
        `lastSeq:  ${lastSeq}\n` +
        `rate:     ${(received / elapsed).toFixed(0)} msg/s\n`;
      lastUpdate = now;
    }

    requestAnimationFrame(pump);
  }
  pump();

  stopBtn.onclick = () => {
    running = false;
    worker.terminate();
    startBtn.disabled = false;
    stopBtn.disabled = true;
  };
};

