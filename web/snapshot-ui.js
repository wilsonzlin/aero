const STATUS_EL = /** @type {HTMLElement} */ (document.getElementById("status"));

const OPFS_SNAPSHOT_FILE = "aero-autosave.snap";

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
  const writable = await handle.createWritable();
  await writable.write(bytes);
  await writable.close();
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
    log(`Save failed: ${err}`);
  }
});

document.getElementById("load").addEventListener("click", async () => {
  try {
    await doLoad();
  } catch (err) {
    log(`Load failed: ${err}`);
  }
});

document.getElementById("export").addEventListener("click", async () => {
  try {
    const bytes = await loadSnapshotFromOpfs();
    downloadSnapshot(bytes);
    log(`Exported snapshot file (${bytes.byteLength} bytes)`);
  } catch (err) {
    log(`Export failed: ${err}`);
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
    log(`Import failed: ${err}`);
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
    doSave().catch((err) => log(`Auto-save failed: ${err}`));
  }, seconds * 1000);
  log(`Auto-save enabled: every ${seconds}s`);
});

log("Snapshot UI loaded. Provide window.aeroSnapshotHost to enable Save/Load.");

