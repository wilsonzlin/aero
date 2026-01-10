const WASM_PAGE_SIZE_BYTES = 64 * 1024;

export const GUEST_RAM_PRESETS = Object.freeze([
  { label: "512 MiB", mib: 512 },
  { label: "1 GiB", mib: 1024 },
  { label: "2 GiB", mib: 2048 },
  { label: "3 GiB", mib: 3072 },
]);

export const DEFAULT_GUEST_RAM_MIB = 1024;

export const PROTOCOL = Object.freeze({
  // Fixed-size messages so the ring buffer can be SPSC and trivial.
  // Layout: [opcode, a0, a1, a2]
  MSG_I32: 4,

  // Opcodes.
  OP_INC32_AT_OFFSET: 1,
});

export const RING_I32 = Object.freeze({
  I_HEAD: 0,
  I_TAIL: 1,
  I_DATA: 2,
});

function isPowerOfTwo(n) {
  return n > 0 && (n & (n - 1)) === 0;
}

export class RingBufferI32 {
  /** @param {Int32Array} backing */
  constructor(backing) {
    this.backing = backing;
    this.capacityI32 = backing.length - RING_I32.I_DATA;
    if (this.capacityI32 <= 0) {
      throw new Error("RingBufferI32 backing array is too small.");
    }
    if (!isPowerOfTwo(this.capacityI32)) {
      throw new Error(`RingBufferI32 capacity must be a power of two, got ${this.capacityI32}.`);
    }
    if (this.capacityI32 % PROTOCOL.MSG_I32 !== 0) {
      throw new Error(
        `RingBufferI32 capacity (${this.capacityI32}) must be a multiple of MSG_I32 (${PROTOCOL.MSG_I32}).`,
      );
    }
    this.mask = this.capacityI32 - 1;
  }

  /** @returns {number} */
  head() {
    return Atomics.load(this.backing, RING_I32.I_HEAD) >>> 0;
  }

  /** @returns {number} */
  tail() {
    return Atomics.load(this.backing, RING_I32.I_TAIL) >>> 0;
  }

  /** @returns {number} */
  availableI32() {
    return (this.head() - this.tail()) >>> 0;
  }

  /** @returns {number} */
  freeI32() {
    return this.capacityI32 - this.availableI32();
  }

  /** @param {number[]} msg */
  pushMessage(msg) {
    if (msg.length !== PROTOCOL.MSG_I32) {
      throw new Error(`pushMessage expected MSG_I32=${PROTOCOL.MSG_I32}, got ${msg.length}`);
    }

    const head = this.head();
    const tail = this.tail();
    const used = (head - tail) >>> 0;
    const free = this.capacityI32 - used;
    if (free < PROTOCOL.MSG_I32) return false;

    for (let i = 0; i < PROTOCOL.MSG_I32; i++) {
      const pos = (head + i) & this.mask;
      this.backing[RING_I32.I_DATA + pos] = msg[i] | 0;
    }

    Atomics.store(this.backing, RING_I32.I_HEAD, (head + PROTOCOL.MSG_I32) >>> 0);
    return true;
  }

  popMessage() {
    const head = this.head();
    const tail = this.tail();
    const available = (head - tail) >>> 0;
    if (available < PROTOCOL.MSG_I32) return null;

    /** @type {number[]} */
    const msg = new Array(PROTOCOL.MSG_I32);
    for (let i = 0; i < PROTOCOL.MSG_I32; i++) {
      const pos = (tail + i) & this.mask;
      msg[i] = this.backing[RING_I32.I_DATA + pos] | 0;
    }

    Atomics.store(this.backing, RING_I32.I_TAIL, (tail + PROTOCOL.MSG_I32) >>> 0);
    return msg;
  }

  /**
   * Wait until new data is available. Only valid in workers (main thread cannot
   * use Atomics.wait).
   */
  waitForDataBlocking(timeoutMs) {
    const head = this.head();
    return Atomics.wait(this.backing, RING_I32.I_HEAD, head, timeoutMs);
  }

  /**
   * Wait until new data is available. Uses Atomics.waitAsync where available,
   * otherwise falls back to polling.
   */
  async waitForDataAsync(timeoutMs) {
    const head = this.head();
    return await waitForStateChangeAsync(this.backing, RING_I32.I_HEAD, head, timeoutMs);
  }

  notifyData() {
    Atomics.notify(this.backing, RING_I32.I_HEAD, 1);
  }
}

export function mibToBytes(mib) {
  return mib * 1024 * 1024;
}

export function bytesToWasmPages(bytes) {
  return Math.ceil(bytes / WASM_PAGE_SIZE_BYTES);
}

export function describeBytes(bytes) {
  const gib = 1024 * 1024 * 1024;
  const mib = 1024 * 1024;
  const kib = 1024;
  if (bytes % gib === 0) return `${bytes / gib} GiB`;
  if (bytes % mib === 0) return `${bytes / mib} MiB`;
  if (bytes % kib === 0) return `${bytes / kib} KiB`;
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
  const ringCapacityI32 = config.ringCapacityI32 ?? 16384; // Must be a power of two.
  if (!isPowerOfTwo(ringCapacityI32)) {
    throw new Error(`ringCapacityI32 must be a power of two, got ${ringCapacityI32}`);
  }
  const cmdSabBytes = (RING_I32.I_DATA + ringCapacityI32) * 4;
  const eventSabBytes = (RING_I32.I_DATA + ringCapacityI32) * 4;

  /** @type {WebAssembly.Memory} */
  let guestMemory;
  try {
    if (guestPages > 65536) {
      throw new Error(
        `Requested guest RAM exceeds wasm32 address space: ${describeBytes(guestRamBytes)} (${guestPages} pages)`,
      );
    }
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
  const eventI32 = new Int32Array(eventSab);

  // Defensive init: clear ring heads/tails (SAB is zeroed, but be explicit).
  Atomics.store(cmdI32, RING_I32.I_HEAD, 0);
  Atomics.store(cmdI32, RING_I32.I_TAIL, 0);
  Atomics.store(eventI32, RING_I32.I_HEAD, 0);
  Atomics.store(eventI32, RING_I32.I_TAIL, 0);

  return {
    config: {
      guestRamMiB,
      guestRamBytes,
      guestPages,
      stateSabBytes,
      cmdSabBytes,
      eventSabBytes,
      ringCapacityI32,
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
      eventI32,
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
