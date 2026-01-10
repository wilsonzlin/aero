const WASM_PAGE_SIZE_BYTES = 64 * 1024;

export const GUEST_RAM_PRESETS = Object.freeze([
  { label: "512 MiB", mib: 512 },
  { label: "1 GiB", mib: 1024 },
  { label: "2 GiB", mib: 2048 },
  { label: "3 GiB", mib: 3072 },
]);

export const DEFAULT_GUEST_RAM_MIB = 1024;

export const CMD = Object.freeze({
  // Int32 indices inside `cmdI32`.
  I_STATE: 0,
  I_OPCODE: 1,
  I_ARG0: 2,
  I_RESULT0: 3,
  I_ERROR: 4,

  // States for `I_STATE`.
  STATE_IDLE: 0,
  STATE_REQUEST: 1,
  STATE_RESPONSE: 2,

  // Opcodes for `I_OPCODE`.
  OP_INC32_AT_OFFSET: 1,
});

export function mibToBytes(mib) {
  return mib * 1024 * 1024;
}

export function bytesToWasmPages(bytes) {
  return Math.ceil(bytes / WASM_PAGE_SIZE_BYTES);
}

export function describeBytes(bytes) {
  const gib = 1024 * 1024 * 1024;
  const mib = 1024 * 1024;
  if (bytes % gib === 0) return `${bytes / gib} GiB`;
  if (bytes % mib === 0) return `${bytes / mib} MiB`;
  return `${bytes} B`;
}

export function getGuestRamPreset(mib) {
  const preset = GUEST_RAM_PRESETS.find((p) => p.mib === mib);
  if (!preset) {
    throw new Error(
      `Invalid guest RAM size: ${mib} MiB (expected one of: ${GUEST_RAM_PRESETS.map((p) => p.mib).join(", ")})`,
    );
  }
  return preset;
}

export function detectBlockingIssues() {
  /** @type {string[]} */
  const issues = [];

  if (typeof WebAssembly === "undefined") {
    issues.push("WebAssembly is not available in this environment.");
    return issues;
  }

  if (typeof SharedArrayBuffer === "undefined") {
    issues.push(
      "SharedArrayBuffer is not available. This usually means the page is not cross-origin isolated (COOP/COEP).",
    );
    return issues;
  }

  if (typeof Atomics === "undefined") {
    issues.push("Atomics is not available (required for WebAssembly threads).");
    return issues;
  }

  if (!globalThis.crossOriginIsolated) {
    issues.push(
      "crossOriginIsolated is false. SharedArrayBuffer + WebAssembly threads require COOP/COEP response headers.",
    );
  }

  // Feature-detect shared wasm memory support with a tiny allocation.
  try {
    // eslint-disable-next-line no-new
    new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  } catch (err) {
    issues.push(
      `This browser rejected shared WebAssembly.Memory (threads). Error: ${stringifyError(err)}`,
    );
  }

  return issues;
}

export function createAeroMemoryModel(config) {
  const guestRamMiB = config.guestRamMiB ?? DEFAULT_GUEST_RAM_MIB;
  getGuestRamPreset(guestRamMiB);

  const issues = detectBlockingIssues();
  if (issues.length > 0) {
    const message =
      "Browser is missing required features for SharedArrayBuffer/WebAssembly threads:\n" +
      issues.map((i) => `- ${i}`).join("\n");
    throw new Error(message);
  }

  const guestRamBytes = mibToBytes(guestRamMiB);
  const guestPages = bytesToWasmPages(guestRamBytes);

  // Small, separate SABs so we never rely on >4GiB offsets.
  const stateSabBytes = 64 * 1024;
  const cmdSabBytes = 64 * 1024;
  const eventSabBytes = 64 * 1024;

  /** @type {WebAssembly.Memory} */
  let guestMemory;
  try {
    guestMemory = new WebAssembly.Memory({
      initial: guestPages,
      maximum: guestPages,
      shared: true,
    });
  } catch (err) {
    throw new Error(
      `Failed to allocate shared WebAssembly.Memory for guest RAM (${describeBytes(guestRamBytes)}). ` +
        `Try a smaller size. Underlying error: ${stringifyError(err)}`,
    );
  }

  const stateSab = new SharedArrayBuffer(stateSabBytes);
  const cmdSab = new SharedArrayBuffer(cmdSabBytes);
  const eventSab = new SharedArrayBuffer(eventSabBytes);

  const guestU8 = new Uint8Array(guestMemory.buffer);
  const guestU32 = new Uint32Array(guestMemory.buffer);

  const stateI32 = new Int32Array(stateSab);
  const cmdI32 = new Int32Array(cmdSab);
  const eventU8 = new Uint8Array(eventSab);

  // Defensive init: ensure cmd starts idle.
  Atomics.store(cmdI32, CMD.I_STATE, CMD.STATE_IDLE);
  Atomics.store(cmdI32, CMD.I_OPCODE, 0);
  Atomics.store(cmdI32, CMD.I_ARG0, 0);
  Atomics.store(cmdI32, CMD.I_RESULT0, 0);
  Atomics.store(cmdI32, CMD.I_ERROR, 0);

  return {
    config: {
      guestRamMiB,
      guestRamBytes,
      guestPages,
      stateSabBytes,
      cmdSabBytes,
      eventSabBytes,
    },
    guestMemory,
    stateSab,
    cmdSab,
    eventSab,
    views: {
      guestU8,
      guestU32,
      stateI32,
      cmdI32,
      eventU8,
    },
  };
}

export async function waitForStateChangeAsync(i32, index, expected, timeoutMs) {
  if (typeof Atomics.waitAsync === "function") {
    const res = Atomics.waitAsync(i32, index, expected, timeoutMs);
    return res.async ? await res.value : res.value;
  }

  const start = performance.now();
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const cur = Atomics.load(i32, index);
    if (cur !== expected) return "not-equal";
    if (timeoutMs != null && performance.now() - start > timeoutMs) return "timed-out";
    await new Promise((r) => setTimeout(r, 0));
  }
}

export function stringifyError(err) {
  if (err instanceof Error) return `${err.name}: ${err.message}`;
  try {
    return String(err);
  } catch {
    return "<unprintable>";
  }
}

