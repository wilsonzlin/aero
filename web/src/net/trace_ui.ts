import { openFileHandle } from "../platform/opfs";

export interface NetTraceBackend {
  isEnabled(): boolean;
  enable(): void;
  disable(): void;
  downloadPcapng(): Promise<Uint8Array>;
}

export function installNetTraceUI(container: HTMLElement, backend: NetTraceBackend): void {
  const wrapper = document.createElement("div");
  wrapper.className = "net-trace";

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
  enableCheckbox.checked = backend.isEnabled();
  enableCheckbox.addEventListener("change", () => {
    status.textContent = "";
    try {
      if (enableCheckbox.checked) {
        backend.enable();
      } else {
        backend.disable();
      }
    } catch (err) {
      enableCheckbox.checked = backend.isEnabled();
      status.textContent = err instanceof Error ? err.message : String(err);
    }
  });
  enableLabel.appendChild(enableCheckbox);
  enableLabel.appendChild(document.createTextNode(" Enable network tracing"));
  wrapper.appendChild(enableLabel);

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
        bytes.buffer instanceof ArrayBuffer ? (bytes as Uint8Array<ArrayBuffer>) : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
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
    } catch (err) {
      status.textContent = err instanceof Error ? err.message : String(err);
    }
  });

  const opfsPath = document.createElement("input");
  opfsPath.type = "text";
  opfsPath.value = "captures/aero-net-trace.pcapng";

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
        bytes.buffer instanceof ArrayBuffer ? (bytes as Uint8Array<ArrayBuffer>) : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
      const handle = await openFileHandle(path, { create: true });
      const writable = await handle.createWritable();
      await writable.write(bytesForIo);
      await writable.close();
      status.textContent = `Saved capture to OPFS: ${path} (${bytes.byteLength.toLocaleString()} bytes)`;
    } catch (err) {
      status.textContent = err instanceof Error ? err.message : String(err);
    }
  });

  const buttonRow = document.createElement("div");
  buttonRow.className = "row";
  buttonRow.appendChild(downloadButton);
  buttonRow.appendChild(saveButton);
  wrapper.appendChild(buttonRow);

  const opfsRow = document.createElement("div");
  opfsRow.className = "row";
  opfsRow.appendChild(document.createTextNode("OPFS path: "));
  opfsRow.appendChild(opfsPath);
  wrapper.appendChild(opfsRow);

  wrapper.appendChild(status);

  container.appendChild(wrapper);
}
