const STATUS_EL = /** @type {HTMLElement} */ (document.getElementById("status"));

const OPFS_SNAPSHOT_FILE = "aero-autosave.snap";

const UTF8 = Object.freeze({ encoding: "utf-8" });
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder(UTF8.encoding);

function formatOneLineUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  if (maxBytes === 0) return "";

  const buf = new Uint8Array(maxBytes);
  let written = 0;
  let pendingSpace = false;
  for (const ch of String(input ?? "")) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = written > 0;
      continue;
    }

    if (pendingSpace) {
      const spaceRes = textEncoder.encodeInto(" ", buf.subarray(written));
      if (spaceRes.written === 0) break;
      written += spaceRes.written;
      pendingSpace = false;
      if (written >= maxBytes) break;
    }

    const res = textEncoder.encodeInto(ch, buf.subarray(written));
    if (res.written === 0) break;
    written += res.written;
    if (written >= maxBytes) break;
  }
  return written === 0 ? "" : textDecoder.decode(buf.subarray(0, written));
}

function safeErrorMessageInput(err) {
  if (err === null) return "null";
  const t = typeof err;
  if (t === "string") return err;
  if (t === "number" || t === "boolean" || t === "bigint" || t === "symbol" || t === "undefined") return String(err);

  if (t === "object") {
    try {
      const msg = err && typeof err.message === "string" ? err.message : null;
      if (msg !== null) return msg;
    } catch {
      // ignore getters throwing
    }
    try {
      const name = err && typeof err.name === "string" ? err.name : null;
      if (name !== null) return name;
    } catch {
      // ignore getters throwing
    }
  }

  return "Error";
}

function formatOneLineError(err, maxBytes) {
  return formatOneLineUtf8(safeErrorMessageInput(err), maxBytes) || "Error";
}

function log(line) {
  STATUS_EL.textContent = `${new Date().toISOString()}  ${line}\n${STATUS_EL.textContent}`;
}

async function getOpfsRoot() {
  if (!("storage" in navigator) || typeof navigator.storage.getDirectory !== "function") {
    throw new Error("OPFS not available (navigator.storage.getDirectory missing)");
  }
  return await navigator.storage.getDirectory();
}

async function saveSnapshotToOpfs(bytes) {
  const root = await getOpfsRoot();
  const handle = await root.getFileHandle(OPFS_SNAPSHOT_FILE, { create: true });
  let writable = null;
  let truncateFallback = false;
  try {
    writable = await handle.createWritable({ keepExistingData: false });
  } catch {
    // Some implementations may not accept options; fall back to default.
    writable = await handle.createWritable();
    truncateFallback = true;
  }
  if (truncateFallback) {
    try {
      if (writable && typeof writable.truncate === "function") await writable.truncate(0);
    } catch {
      // ignore
    }
  }
  try {
    await writable.write(bytes);
    await writable.close();
  } catch (err) {
    // Abort on error so a failed write does not leave behind a truncated/partial snapshot file.
    try {
      if (writable && typeof writable.abort === "function") await writable.abort(err);
    } catch {
      // ignore
    }
    throw err;
  }
}

async function loadSnapshotFromOpfs() {
  const root = await getOpfsRoot();
  const handle = await root.getFileHandle(OPFS_SNAPSHOT_FILE, { create: false });
  const file = await handle.getFile();
  return new Uint8Array(await file.arrayBuffer());
}

function downloadSnapshot(bytes, filename = "aero-snapshot.snap") {
  const blob = new Blob([bytes], { type: "application/octet-stream" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

// These are expected to be provided by the WASM host integration.
// - saveSnapshotBytes(): Promise<Uint8Array>
// - loadSnapshotBytes(bytes: Uint8Array): Promise<void>
function requireHostApi() {
  const api = /** @type {any} */ (globalThis).aeroSnapshotHost;
  if (!api || typeof api.saveSnapshotBytes !== "function" || typeof api.loadSnapshotBytes !== "function") {
    throw new Error(
      "Missing host API. Expected window.aeroSnapshotHost.saveSnapshotBytes() and loadSnapshotBytes(bytes)."
    );
  }
  return api;
}

let autosaveTimer = null;

async function doSave() {
  const api = requireHostApi();
  const bytes = await api.saveSnapshotBytes();
  await saveSnapshotToOpfs(bytes);
  log(`Saved snapshot to OPFS (${bytes.byteLength} bytes)`);
  return bytes;
}

async function doLoad(bytesOverride = null) {
  const api = requireHostApi();
  const bytes = bytesOverride ?? (await loadSnapshotFromOpfs());
  await api.loadSnapshotBytes(bytes);
  log(`Loaded snapshot (${bytes.byteLength} bytes)`);
}

document.getElementById("save").addEventListener("click", async () => {
  try {
    await doSave();
  } catch (err) {
    log(`Save failed: ${formatOneLineError(err, 512)}`);
  }
});

document.getElementById("load").addEventListener("click", async () => {
  try {
    await doLoad();
  } catch (err) {
    log(`Load failed: ${formatOneLineError(err, 512)}`);
  }
});

document.getElementById("export").addEventListener("click", async () => {
  try {
    const bytes = await loadSnapshotFromOpfs();
    downloadSnapshot(bytes);
    log(`Exported snapshot file (${bytes.byteLength} bytes)`);
  } catch (err) {
    log(`Export failed: ${formatOneLineError(err, 512)}`);
  }
});

document.getElementById("import").addEventListener("change", async (ev) => {
  try {
    const input = /** @type {HTMLInputElement} */ (ev.target);
    if (!input.files || input.files.length === 0) return;
    const file = input.files[0];
    const bytes = new Uint8Array(await file.arrayBuffer());
    await doLoad(bytes);
    await saveSnapshotToOpfs(bytes);
    log(`Imported snapshot and persisted to OPFS (${bytes.byteLength} bytes)`);
  } catch (err) {
    log(`Import failed: ${formatOneLineError(err, 512)}`);
  }
});

document.getElementById("autosave").addEventListener("change", async (ev) => {
  const input = /** @type {HTMLInputElement} */ (ev.target);
  const seconds = Number.parseInt(input.value, 10);
  if (!Number.isFinite(seconds) || seconds < 0) {
    input.value = "0";
    return;
  }

  if (autosaveTimer) {
    clearInterval(autosaveTimer);
    autosaveTimer = null;
  }

  if (seconds === 0) {
    log("Auto-save disabled");
    return;
  }

  autosaveTimer = setInterval(() => {
    doSave().catch((err) => log(`Auto-save failed: ${formatOneLineError(err, 512)}`));
  }, seconds * 1000);
  autosaveTimer?.unref?.();
  log(`Auto-save enabled: every ${seconds}s`);
});

log("Snapshot UI loaded. Provide window.aeroSnapshotHost to enable Save/Load.");
