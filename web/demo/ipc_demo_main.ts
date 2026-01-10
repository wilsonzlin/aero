import { alignUp, ringCtrl } from "../src/ipc/layout";
import { decodeEvent, encodeCommand } from "../src/ipc/protocol";
import { RingBuffer } from "../src/ipc/ring_buffer";

const startBtn = document.getElementById("start") as HTMLButtonElement | null;
const stopBtn = document.getElementById("stop") as HTMLButtonElement | null;
const statsEl = document.getElementById("stats") as HTMLDivElement | null;

if (!startBtn || !stopBtn || !statsEl) {
  throw new Error("Missing demo DOM elements");
}

let running = false;
let worker: Worker | null = null;

startBtn.onclick = () => {
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

  worker = new Worker(new URL("./ipc_demo_worker.ts", import.meta.url), { type: "module" });
  worker.postMessage({ sab, cmdOffset, evtOffset });

  let sent = 0;
  let received = 0;
  let lastSeq = 0;
  const startTime = performance.now();
  let lastUpdate = startTime;

  const pump = () => {
    if (!running) return;

    // Send as much as we can each frame.
    for (let i = 0; i < 50_000; i++) {
      if (!cmdQ.tryPush(encodeCommand({ kind: "nop", seq: sent }))) break;
      sent++;
    }

    // Drain events.
    for (;;) {
      const msg = evtQ.tryPop();
      if (!msg) break;
      const evt = decodeEvent(msg);
      if (evt.kind === "ack") {
        lastSeq = evt.seq;
        received++;
      }
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
  };
  pump();
};

stopBtn.onclick = () => {
  if (!running) return;
  running = false;
  worker?.terminate();
  worker = null;
  startBtn.disabled = false;
  stopBtn.disabled = true;
  statsEl.textContent = "stopped";
};

