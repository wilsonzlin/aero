import {
  CMD,
  DEFAULT_GUEST_RAM_MIB,
  GUEST_RAM_PRESETS,
  createAeroMemoryModel,
  describeBytes,
  stringifyError,
  waitForStateChangeAsync,
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

startBtn.addEventListener("click", async () => {
  clearLog();

  const guestRamMiB = Number(ramSelect.value);
  log(`Allocating memory model (guest RAM = ${guestRamMiB} MiB)...`);

  try {
    model = createAeroMemoryModel({ guestRamMiB });
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
  log(`- stateSab: ${describeBytes(model.config.stateSabBytes)}`);
  log(`- cmdSab: ${describeBytes(model.config.cmdSabBytes)}`);
  log(`- eventSab: ${describeBytes(model.config.eventSabBytes)}`);

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
  if (!model) return;

  const { guestU32, cmdI32 } = model.views;

  // Put a value in guest RAM that the worker will mutate.
  guestU32[0] = 41;
  log(`main: wrote guestU32[0] = ${guestU32[0]}`);

  Atomics.store(cmdI32, CMD.I_OPCODE, CMD.OP_INC32_AT_OFFSET);
  Atomics.store(cmdI32, CMD.I_ARG0, 0); // byte offset
  Atomics.store(cmdI32, CMD.I_STATE, CMD.STATE_REQUEST);
  Atomics.notify(cmdI32, CMD.I_STATE, 1);

  const status = await waitForStateChangeAsync(cmdI32, CMD.I_STATE, CMD.STATE_REQUEST, 2000);
  const stateAfter = Atomics.load(cmdI32, CMD.I_STATE);
  log(`main: wait status = ${status}, cmd state = ${stateAfter}`);

  if (stateAfter !== CMD.STATE_RESPONSE) {
    log("main: ERROR: expected worker response (STATE_RESPONSE).");
    return;
  }

  const result = Atomics.load(cmdI32, CMD.I_RESULT0);
  const errCode = Atomics.load(cmdI32, CMD.I_ERROR);
  log(`main: worker result = ${result}, error = ${errCode}`);
  log(`main: after worker ran, guestU32[0] = ${guestU32[0]}`);

  // Reset to idle so the worker can wait for the next command.
  Atomics.store(cmdI32, CMD.I_STATE, CMD.STATE_IDLE);
  Atomics.notify(cmdI32, CMD.I_STATE, 1);
});

