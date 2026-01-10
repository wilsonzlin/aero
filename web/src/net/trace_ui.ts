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

  const enableLabel = document.createElement("label");
  const enableCheckbox = document.createElement("input");
  enableCheckbox.type = "checkbox";
  enableCheckbox.checked = backend.isEnabled();
  enableCheckbox.addEventListener("change", () => {
    if (enableCheckbox.checked) {
      backend.enable();
    } else {
      backend.disable();
    }
  });
  enableLabel.appendChild(enableCheckbox);
  enableLabel.appendChild(document.createTextNode(" Enable network tracing"));
  wrapper.appendChild(enableLabel);

  const downloadButton = document.createElement("button");
  downloadButton.textContent = "Download capture (PCAPNG)";
  downloadButton.addEventListener("click", async () => {
    const bytes = await backend.downloadPcapng();
    const blob = new Blob([bytes], { type: "application/vnd.tcpdump.pcap" });
    const url = URL.createObjectURL(blob);
    try {
      const a = document.createElement("a");
      a.href = url;
      a.download = `aero-net-${new Date().toISOString().replace(/[:.]/g, "-")}.pcapng`;
      a.click();
    } finally {
      URL.revokeObjectURL(url);
    }
  });
  wrapper.appendChild(downloadButton);

  container.appendChild(wrapper);
}

