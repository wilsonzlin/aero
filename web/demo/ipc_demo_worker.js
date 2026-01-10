// Worker side of the IPC demo.

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

      while (u32(Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT)) !== tail) {
        const cur = Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT);
        Atomics.wait(this.ctrl, ringCtrl.TAIL_COMMIT, cur);
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

  waitForData() {
    while (Atomics.load(this.ctrl, ringCtrl.HEAD) === Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT)) {
      const tail = Atomics.load(this.ctrl, ringCtrl.TAIL_COMMIT);
      Atomics.wait(this.ctrl, ringCtrl.TAIL_COMMIT, tail);
    }
  }
}

function decodeNop(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const tag = view.getUint16(0, true);
  if (tag !== 0x0000) throw new Error("not nop");
  return view.getUint32(2, true);
}

function encodeAck(seq) {
  const out = new Uint8Array(2 + 4);
  const view = new DataView(out.buffer);
  view.setUint16(0, 0x1000, true);
  view.setUint32(2, seq >>> 0, true);
  return out;
}

let cmdQ;
let evtQ;

self.onmessage = (ev) => {
  const { sab, cmdOffset, evtOffset } = ev.data;
  cmdQ = new RingBuffer(sab, cmdOffset);
  evtQ = new RingBuffer(sab, evtOffset);

  // Main loop: block for data, respond with ack.
  for (;;) {
    cmdQ.waitForData();
    for (;;) {
      const msg = cmdQ.tryPop();
      if (!msg) break;
      const seq = decodeNop(msg);
      // Spin if event queue is full (demo).
      while (!evtQ.tryPush(encodeAck(seq))) {}
    }
  }
};

