import "./style.css";

import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from "./platform/features";
import { createAudioOutput } from "./platform/audio";
import { importFileToOpfs } from "./platform/opfs";
import { requestWebGpuDevice } from "./platform/webgpu";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, unknown> = {},
  ...children: Array<Node | string | null | undefined>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined) continue;
    if (key === "class") {
      node.className = String(value);
    } else if (key === "text") {
      node.textContent = String(value);
    } else if (key.startsWith("on") && typeof value === "function") {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (node as any)[key.toLowerCase()] = value;
    } else {
      node.setAttribute(key, String(value));
    }
  }
  for (const child of children) {
    if (child === null || child === undefined) continue;
    node.append(child instanceof Node ? child : document.createTextNode(child));
  }
  return node;
}

function render(): void {
  const app = document.getElementById("app");
  if (!app) throw new Error("Missing #app element");

  const report = detectPlatformFeatures();
  const missing = explainMissingRequirements(report);

  app.replaceChildren(
    el("h1", { text: "Aero Platform Capabilities" }),
    el(
      "div",
      { class: `panel ${missing.length ? "missing" : ""}` },
      el("h2", { text: "Required features" }),
      missing.length
        ? el(
            "ul",
            {},
            ...missing.map((m) => el("li", { text: m })),
          )
        : el("div", { text: "All required features appear to be available." }),
    ),
    el(
      "div",
      { class: "panel" },
      el("h2", { text: "Capability report" }),
      renderCapabilityTable(report),
    ),
    renderWebGpuPanel(),
    renderOpfsPanel(),
    renderAudioPanel(),
  );
}

function renderCapabilityTable(report: PlatformFeatureReport): HTMLTableElement {
  const orderedKeys: Array<keyof PlatformFeatureReport> = [
    "crossOriginIsolated",
    "sharedArrayBuffer",
    "wasmSimd",
    "wasmThreads",
    "webgpu",
    "opfs",
    "audioWorklet",
    "offscreenCanvas",
  ];

  const tbody = el("tbody");
  for (const key of orderedKeys) {
    const val = report[key];
    tbody.append(
      el(
        "tr",
        {},
        el("th", { text: key }),
        el("td", { class: val ? "ok" : "bad", text: val ? "supported" : "missing" }),
      ),
    );
  }

  return el("table", {}, el("thead", {}, el("tr", {}, el("th", { text: "feature" }), el("th", { text: "status" }))), tbody);
}

function renderWebGpuPanel(): HTMLElement {
  const output = el("pre", { text: "" });
  const button = el("button", {
    text: "Request WebGPU device",
    onclick: async () => {
      output.textContent = "";
      try {
        const { adapter, preferredFormat } = await requestWebGpuDevice({ powerPreference: "high-performance" });
        output.textContent = JSON.stringify(
          {
            adapterInfo: "requestAdapter succeeded",
            features: Array.from(adapter.features.values()),
            preferredFormat,
          },
          null,
          2,
        );
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  return el("div", { class: "panel" }, el("h2", { text: "WebGPU" }), el("div", { class: "row" }, button), output);
}

function renderOpfsPanel(): HTMLElement {
  const status = el("pre", { text: "" });
  const progress = el("progress", { value: "0", max: "1", style: "width: 320px" }) as HTMLProgressElement;
  const destPathInput = el("input", { type: "text", value: "images/disk.img" }) as HTMLInputElement;
  const fileInput = el("input", { type: "file" }) as HTMLInputElement;

  const importButton = el("button", {
    text: "Import to OPFS",
    onclick: async () => {
      status.textContent = "";
      progress.value = 0;
      const file = fileInput.files?.[0];
      if (!file) {
        status.textContent = "Pick a file first.";
        return;
      }
      const destPath = destPathInput.value.trim();
      if (!destPath) {
        status.textContent = "Destination path must not be empty.";
        return;
      }

      try {
        await importFileToOpfs(file, destPath, ({ writtenBytes, totalBytes }) => {
          progress.value = totalBytes ? writtenBytes / totalBytes : 0;
          status.textContent = `Writing ${writtenBytes.toLocaleString()} / ${totalBytes.toLocaleString()} bytes…`;
        });
        status.textContent = `Imported to OPFS: ${destPath}`;
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  fileInput.addEventListener("change", () => {
    const file = fileInput.files?.[0];
    if (file) destPathInput.value = `images/${file.name}`;
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "OPFS (disk image import)" }),
    el(
      "div",
      { class: "row" },
      el("label", { text: "File:" }),
      fileInput,
      el("label", { text: "Dest path:" }),
      destPathInput,
      importButton,
      progress,
    ),
    status,
  );
}

function renderAudioPanel(): HTMLElement {
  const status = el("pre", { text: "" });
  const button = el("button", {
    text: "Init audio output",
    onclick: async () => {
      status.textContent = "";
      const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: "interactive" });
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }
      try {
        await output.resume();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }
      status.textContent =
        "Audio initialized. Ring buffer allocated; processor is a stub (will output silence until samples are written).";
    },
  });

  return el("div", { class: "panel" }, el("h2", { text: "Audio" }), el("div", { class: "row" }, button), status);
}

render();
