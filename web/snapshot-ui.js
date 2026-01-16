import { formatOneLineError as formatOneLineErrorShared } from "./_shared/text_one_line.js";

function byId(id, ctor) {
  const el = document.getElementById(id);
  if (!(el instanceof ctor)) {
    const name = ctor && typeof ctor === "function" ? ctor.name : "HTMLElement";
    throw new Error(`snapshot-ui: missing required element #${id} (${name})`);
  }
  return el;
}

const STATUS_EL = byId("status", HTMLElement);
const SAVE_BTN = byId("save", HTMLButtonElement);
const LOAD_BTN = byId("load", HTMLButtonElement);
const EXPORT_BTN = byId("export", HTMLButtonElement);
const IMPORT_INPUT = byId("import", HTMLInputElement);
const AUTOSAVE_INPUT = byId("autosave", HTMLInputElement);

const OPFS_SNAPSHOT_FILE = "aero-autosave.snap";
const MAX_LOG_LINES = 200;
const logLines = [];

const ERROR_FMT_OPTS = Object.freeze({ includeNameFallback: "missing" });

function formatOneLineError(err, maxBytes) {
  return formatOneLineErrorShared(err, maxBytes, ERROR_FMT_OPTS);
}

function log(line) {
  logLines.unshift(`${new Date().toISOString()}  ${line}`);
  if (logLines.length > MAX_LOG_LINES) logLines.length = MAX_LOG_LINES;
  STATUS_EL.textContent = logLines.join("\n");
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
      if (writable && typeof writable.abort === "function") await writable.abort();
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
  const parent = document.body ?? document.documentElement;
  parent.appendChild(a);
  try {
    a.click();
  } finally {
    a.remove();
    // Avoid revoking immediately; some browsers may still be consuming the URL after click.
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }
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

SAVE_BTN.addEventListener("click", async () => {
  try {
    await doSave();
  } catch (err) {
    log(`Save failed: ${formatOneLineError(err, 512)}`);
  }
});

LOAD_BTN.addEventListener("click", async () => {
  try {
    await doLoad();
  } catch (err) {
    log(`Load failed: ${formatOneLineError(err, 512)}`);
  }
});

EXPORT_BTN.addEventListener("click", async () => {
  try {
    const bytes = await loadSnapshotFromOpfs();
    downloadSnapshot(bytes);
    log(`Exported snapshot file (${bytes.byteLength} bytes)`);
  } catch (err) {
    log(`Export failed: ${formatOneLineError(err, 512)}`);
  }
});

IMPORT_INPUT.addEventListener("change", async () => {
  try {
    if (!IMPORT_INPUT.files || IMPORT_INPUT.files.length === 0) return;
    const file = IMPORT_INPUT.files[0];
    const bytes = new Uint8Array(await file.arrayBuffer());
    await doLoad(bytes);
    await saveSnapshotToOpfs(bytes);
    log(`Imported snapshot and persisted to OPFS (${bytes.byteLength} bytes)`);
    IMPORT_INPUT.value = "";
  } catch (err) {
    log(`Import failed: ${formatOneLineError(err, 512)}`);
  }
});

AUTOSAVE_INPUT.addEventListener("change", async () => {
  const seconds = Number.parseInt(AUTOSAVE_INPUT.value, 10);
  if (!Number.isFinite(seconds) || seconds < 0) {
    AUTOSAVE_INPUT.value = "0";
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
