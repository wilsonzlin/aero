import {
  DEFAULT_GUEST_RAM_MIB,
  GUEST_RAM_PRESETS,
  PROTOCOL,
  RingBufferI32,
  createAeroMemoryModel,
  describeBytes,
  stringifyError,
} from "./memory-model.js";

const ramSelect = /** @type {HTMLSelectElement} */ (document.getElementById("ram"));
const startBtn = /** @type {HTMLButtonElement} */ (document.getElementById("start"));
const runBtn = /** @type {HTMLButtonElement} */ (document.getElementById("run"));
const logEl = /** @type {HTMLDivElement} */ (document.getElementById("log"));

function log(line) {
  logEl.textContent += `${line}\n`;
  logEl.scrollTop = logEl.scrollHeight;
}

function clearLog() {
  logEl.textContent = "";
}

for (const preset of GUEST_RAM_PRESETS) {
  const opt = document.createElement("option");
  opt.value = String(preset.mib);
  opt.textContent = preset.label;
  if (preset.mib === DEFAULT_GUEST_RAM_MIB) opt.selected = true;
  ramSelect.appendChild(opt);
}

log(`crossOriginIsolated: ${String(globalThis.crossOriginIsolated)}`);
log(`SharedArrayBuffer: ${typeof SharedArrayBuffer !== "undefined" ? "available" : "missing"}`);
log(`Atomics.waitAsync: ${typeof Atomics?.waitAsync === "function" ? "available" : "missing (polling fallback)"}`);

/** @type {ReturnType<typeof createAeroMemoryModel> | null} */
let model = null;
/** @type {Worker | null} */
let worker = null;
/** @type {RingBufferI32 | null} */
let cmdRing = null;
/** @type {RingBufferI32 | null} */
let eventRing = null;

startBtn.addEventListener("click", async () => {
  clearLog();

  const guestRamMiB = Number(ramSelect.value);
  log(`Allocating memory model (guest RAM = ${guestRamMiB} MiB)...`);

  try {
    model = createAeroMemoryModel({ guestRamMiB });
    cmdRing = new RingBufferI32(model.views.cmdI32);
    eventRing = new RingBufferI32(model.views.eventI32);
  } catch (err) {
    log(`ERROR: ${stringifyError(err)}`);
    log("");
    log("Tip: ensure you are using the provided dev server so COOP/COEP headers are set.");
    log("Tip: try a smaller guest RAM size if allocation fails.");
    runBtn.disabled = true;
    return;
  }

  log("Allocation OK.");
  log(`- guestMemory: ${describeBytes(model.config.guestRamBytes)} (${model.config.guestPages} wasm pages)`);
  log(`- guestMemory.buffer is SharedArrayBuffer: ${model.guestMemory.buffer instanceof SharedArrayBuffer}`);
  log(`- stateSab: ${describeBytes(model.config.stateSabBytes)}`);
  log(`- cmdSab: ${describeBytes(model.config.cmdSabBytes)}`);
  log(`- eventSab: ${describeBytes(model.config.eventSabBytes)}`);
  log(`- ringCapacityI32: ${model.config.ringCapacityI32} (MSG_I32=${PROTOCOL.MSG_I32})`);

  if (worker) worker.terminate();
  worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  worker.onmessage = (ev) => {
    const msg = ev.data;
    if (msg?.type === "log") {
      log(`[worker] ${msg.line}`);
      return;
    }
    log(`[worker] message: ${JSON.stringify(msg)}`);
  };
  worker.onerror = (ev) => {
    log(`[worker] ERROR: ${ev.message}`);
  };

  worker.postMessage({
    type: "init",
    guestMemory: model.guestMemory,
    stateSab: model.stateSab,
    cmdSab: model.cmdSab,
    eventSab: model.eventSab,
  });

  runBtn.disabled = false;
});

runBtn.addEventListener("click", async () => {
  if (!model || !cmdRing || !eventRing) return;

  const { guestU32 } = model.views;

  // Put a value in guest RAM that the worker will mutate.
  guestU32[0] = 41;
  log(`main: wrote guestU32[0] = ${guestU32[0]}`);

  // Command message: [opcode, a0(byteOffset), a1, a2]
  if (!cmdRing.pushMessage([PROTOCOL.OP_INC32_AT_OFFSET, 0, 0, 0])) {
    log("main: ERROR: cmd ring is full");
    return;
  }
  cmdRing.notifyData();

  // Wait for a response event.
  /** @type {number[] | null} */
  let evt = null;
  const timeoutMs = 2000;
  const start = performance.now();
  while (evt === null) {
    evt = eventRing.popMessage();
    if (evt) break;

    const remaining = Math.max(0, timeoutMs - (performance.now() - start));
    const status = await eventRing.waitForDataAsync(remaining);
    if (status === "timed-out") {
      log("main: ERROR: timed out waiting for worker event");
      return;
    }
  }

  const [opcode, result0, error] = evt;
  log(`main: got event opcode=${opcode}, result0=${result0}, error=${error}`);
  log(`main: after worker ran, guestU32[0] = ${guestU32[0]}`);
});
