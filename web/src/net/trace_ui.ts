import { openFileHandle } from "../platform/opfs.ts";
import { formatOneLineError } from "../text";

export interface NetTraceStats {
  enabled: boolean;
  records: number;
  bytes: number;
  droppedRecords?: number;
  droppedBytes?: number;
}

export interface NetTraceBackend {
  isEnabled(): boolean;
  enable(): void;
  disable(): void;
  downloadPcapng(): Promise<Uint8Array>;
  // Non-draining snapshot export (optional).
  exportPcapng?(): Promise<Uint8Array>;

  clear?(): void | Promise<void>;
  getStats?(): NetTraceStats | Promise<NetTraceStats>;

  // Legacy clear implementation used by earlier backends / UIs.
  clearCapture?(): void;
}

export function installNetTraceUI(container: HTMLElement, backend: NetTraceBackend): void {
  const wrapper = document.createElement("div");
  wrapper.className = "net-trace";

  let refreshStats: (() => Promise<void>) | undefined;

  const warning = document.createElement("p");
  warning.textContent =
    "Network captures may contain sensitive data (credentials, cookies, private traffic). " +
    "Enable only when debugging, and handle exported files carefully.";
  wrapper.appendChild(warning);

  const status = document.createElement("pre");
  status.className = "mono";
  status.textContent = "";

  const enableLabel = document.createElement("label");
  const enableCheckbox = document.createElement("input");
  enableCheckbox.type = "checkbox";
  try {
    enableCheckbox.checked = backend.isEnabled();
  } catch (err) {
    status.textContent = formatOneLineError(err, 512);
    enableCheckbox.checked = false;
  }
  enableCheckbox.addEventListener("change", () => {
    status.textContent = "";
    const previousChecked = !enableCheckbox.checked;
    try {
      if (enableCheckbox.checked) {
        backend.enable();
      } else {
        backend.disable();
      }
      void refreshStats?.();
    } catch (err) {
      try {
        enableCheckbox.checked = backend.isEnabled();
      } catch {
        enableCheckbox.checked = previousChecked;
      }
      status.textContent = formatOneLineError(err, 512);
    }
  });
  enableLabel.appendChild(enableCheckbox);
  enableLabel.appendChild(document.createTextNode(" Enable network tracing"));
  wrapper.appendChild(enableLabel);

  const statsLine = backend.getStats ? document.createElement("pre") : null;
  if (statsLine) {
    statsLine.className = "mono";
    statsLine.textContent = "";
    wrapper.appendChild(statsLine);
  }

  const downloadButton = document.createElement("button");
  downloadButton.textContent = "Download capture (PCAPNG)";
  downloadButton.addEventListener("click", async () => {
    status.textContent = "";
    try {
      const bytes = await backend.downloadPcapng();
      // `BlobPart` types only accept ArrayBuffer-backed views; `Uint8Array` is
      // generic over `ArrayBufferLike` and may be backed by `SharedArrayBuffer`.
      // Copy when needed so TypeScript (and spec compliance) are happy.
      const bytesForIo: Uint8Array<ArrayBuffer> =
        bytes.buffer instanceof ArrayBuffer
          ? (bytes as Uint8Array<ArrayBuffer>)
          : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
      const blob = new Blob([bytesForIo], { type: "application/vnd.tcpdump.pcap" });
      const url = URL.createObjectURL(blob);
      try {
        const a = document.createElement("a");
        a.href = url;
        a.download = `aero-net-${new Date().toISOString().replace(/[:.]/g, "-")}.pcapng`;
        a.click();
      } finally {
        URL.revokeObjectURL(url);
      }
      await refreshStats?.();
    } catch (err) {
      status.textContent = formatOneLineError(err, 512);
    }
  });

  const downloadSnapshotButton = backend.exportPcapng ? document.createElement("button") : null;
  if (downloadSnapshotButton) {
    downloadSnapshotButton.textContent = "Download snapshot (PCAPNG)";
    downloadSnapshotButton.addEventListener("click", async () => {
      status.textContent = "";
      try {
        const bytes = await backend.exportPcapng!();
        const bytesForIo: Uint8Array<ArrayBuffer> =
          bytes.buffer instanceof ArrayBuffer
            ? (bytes as Uint8Array<ArrayBuffer>)
            : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
        const blob = new Blob([bytesForIo], { type: "application/vnd.tcpdump.pcap" });
        const url = URL.createObjectURL(blob);
        try {
          const a = document.createElement("a");
          a.href = url;
          a.download = `aero-net-snapshot-${new Date().toISOString().replace(/[:.]/g, "-")}.pcapng`;
          a.click();
        } finally {
          URL.revokeObjectURL(url);
        }
        await refreshStats?.();
      } catch (err) {
        status.textContent = formatOneLineError(err, 512);
      }
    });
  }

  const opfsPath = document.createElement("input");
  opfsPath.type = "text";
  opfsPath.value = "captures/aero-net-trace.pcapng";

  const clearButton = backend.clear || backend.clearCapture ? document.createElement("button") : null;
  if (clearButton) {
    clearButton.textContent = "Clear capture";
    clearButton.addEventListener("click", async () => {
      status.textContent = "";
      try {
        if (backend.clear) {
          await backend.clear();
        } else {
          backend.clearCapture?.();
        }
        await refreshStats?.();
        status.textContent = "Capture cleared.";
      } catch (err) {
        status.textContent = formatOneLineError(err, 512);
      }
    });
  }

  const saveButton = document.createElement("button");
  saveButton.textContent = "Save capture to OPFS";
  saveButton.addEventListener("click", async () => {
    status.textContent = "";
    try {
      const path = opfsPath.value.trim();
      if (!path) {
        throw new Error("OPFS path must not be empty.");
      }

      const bytes = await backend.downloadPcapng();
      const bytesForIo: Uint8Array<ArrayBuffer> =
        bytes.buffer instanceof ArrayBuffer
          ? (bytes as Uint8Array<ArrayBuffer>)
          : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
      const handle = await openFileHandle(path, { create: true });
      let writable: FileSystemWritableFileStream;
      let truncateFallback = false;
      try {
        writable = await handle.createWritable({ keepExistingData: false });
      } catch {
        // Some implementations may not accept options; fall back to default.
        writable = await handle.createWritable();
        truncateFallback = true;
      }
      if (truncateFallback) {
        // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
        // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
        try {
          await writable.truncate(0);
        } catch {
          // ignore
        }
      }
      try {
        await writable.write(bytesForIo);
        await writable.close();
      } catch (err) {
        try {
          await writable.abort(err);
        } catch {
          // ignore abort failures
        }
        throw err;
      }
      status.textContent = `Saved capture to OPFS: ${path} (${bytes.byteLength.toLocaleString()} bytes)`;
      await refreshStats?.();
    } catch (err) {
      status.textContent = formatOneLineError(err, 512);
    }
  });

  const saveSnapshotButton = backend.exportPcapng ? document.createElement("button") : null;
  if (saveSnapshotButton) {
    saveSnapshotButton.textContent = "Save snapshot to OPFS";
    saveSnapshotButton.addEventListener("click", async () => {
      status.textContent = "";
      try {
        const path = opfsPath.value.trim();
        if (!path) {
          throw new Error("OPFS path must not be empty.");
        }

        const bytes = await backend.exportPcapng!();
        const bytesForIo: Uint8Array<ArrayBuffer> =
          bytes.buffer instanceof ArrayBuffer
            ? (bytes as Uint8Array<ArrayBuffer>)
            : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
        const handle = await openFileHandle(path, { create: true });
        let writable: FileSystemWritableFileStream;
        let truncateFallback = false;
        try {
          writable = await handle.createWritable({ keepExistingData: false });
        } catch {
          // Some implementations may not accept options; fall back to default.
          writable = await handle.createWritable();
          truncateFallback = true;
        }
        if (truncateFallback) {
          // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
          // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
          try {
            await writable.truncate(0);
          } catch {
            // ignore
          }
        }
        try {
          await writable.write(bytesForIo);
          await writable.close();
        } catch (err) {
          try {
            await writable.abort(err);
          } catch {
            // ignore abort failures
          }
          throw err;
        }
        status.textContent = `Saved snapshot to OPFS: ${path} (${bytes.byteLength.toLocaleString()} bytes)`;
        // Snapshot does not drain; still refresh in case the backend updated stats.
        await refreshStats?.();
      } catch (err) {
      status.textContent = formatOneLineError(err, 512);
      }
    });
  }

  const buttonRow = document.createElement("div");
  buttonRow.className = "row";
  if (clearButton) {
    buttonRow.appendChild(clearButton);
  }
  buttonRow.appendChild(downloadButton);
  if (downloadSnapshotButton) {
    buttonRow.appendChild(downloadSnapshotButton);
  }
  buttonRow.appendChild(saveButton);
  if (saveSnapshotButton) {
    buttonRow.appendChild(saveSnapshotButton);
  }
  wrapper.appendChild(buttonRow);

  const opfsRow = document.createElement("div");
  opfsRow.className = "row";
  opfsRow.appendChild(document.createTextNode("OPFS path: "));
  opfsRow.appendChild(opfsPath);
  wrapper.appendChild(opfsRow);

  wrapper.appendChild(status);

  container.appendChild(wrapper);

  if (backend.getStats && statsLine) {
    const lifetime = new AbortController();
    const { signal } = lifetime;
    const getStats = backend.getStats.bind(backend);

    const formatStats = (stats: NetTraceStats): string => {
      let line =
        `enabled=${stats.enabled ? "yes" : "no"} ` +
        `records=${stats.records.toLocaleString()} ` +
        `bytes=${stats.bytes.toLocaleString()}`;
      if (stats.droppedRecords !== undefined) {
        line += ` droppedRecords=${stats.droppedRecords.toLocaleString()}`;
      }
      if (stats.droppedBytes !== undefined) {
        line += ` droppedBytes=${stats.droppedBytes.toLocaleString()}`;
      }
      return line;
    };

    let statsRequestInFlight = false;
    let everConnected = false;
    const pollStats = async (): Promise<void> => {
      if (signal.aborted) return;

      if (wrapper.isConnected) {
        everConnected = true;
      } else if (everConnected) {
        lifetime.abort();
        return;
      } else {
        return;
      }

      if (statsRequestInFlight) return;
      statsRequestInFlight = true;
      try {
        const stats = await getStats();
        if (signal.aborted) return;
        statsLine.textContent = formatStats(stats);
        enableCheckbox.checked = stats.enabled;
      } catch (err) {
        if (signal.aborted) return;
        status.textContent = formatOneLineError(err, 512);
      } finally {
        statsRequestInFlight = false;
      }
    };

    refreshStats = pollStats;

    const timerId = window.setInterval(() => {
      void pollStats();
    }, 500);

    const onPageHide = () => {
      lifetime.abort();
    };
    const onBeforeUnload = () => {
      lifetime.abort();
    };
    window.addEventListener("pagehide", onPageHide);
    window.addEventListener("beforeunload", onBeforeUnload);

    signal.addEventListener(
      "abort",
      () => {
        window.clearInterval(timerId);
        window.removeEventListener("pagehide", onPageHide);
        window.removeEventListener("beforeunload", onBeforeUnload);
      },
      { once: true },
    );

    void pollStats();
  }
}
