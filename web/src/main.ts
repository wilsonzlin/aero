import "./style.css";

import { createAudioOutput } from "./platform/audio";
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from "./platform/features";
import { importFileToOpfs } from "./platform/opfs";
import { requestWebGpuDevice } from "./platform/webgpu";
import { installPerfHud } from "./perf/hud_entry";
import { WorkerCoordinator } from "./runtime/coordinator";
import { DEFAULT_GUEST_RAM_MIB, GUEST_RAM_PRESETS_MIB, type GuestRamMiB } from "./runtime/shared_layout";

installPerfHud({ guestRamBytes: DEFAULT_GUEST_RAM_MIB * 1024 * 1024 });

const workerCoordinator = new WorkerCoordinator();

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
    renderWorkersPanel(report),
    renderIpcDemoPanel(),
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

  return el(
    "table",
    {},
    el("thead", {}, el("tr", {}, el("th", { text: "feature" }), el("th", { text: "status" }))),
    tbody,
  );
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
  let toneTimer: number | null = null;
  let tonePhase = 0;

  function stopTone() {
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
  }

  function startTone(output: Exclude<Awaited<ReturnType<typeof createAudioOutput>>, { enabled: false }>) {
    stopTone();

    const freqHz = 440;
    const gain = 0.1;
    const channelCount = output.ringBuffer.channelCount;
    const sr = output.context.sampleRate;

    function writeTone(frames: number) {
      const buf = new Float32Array(frames * channelCount);
      for (let i = 0; i < frames; i++) {
        const s = Math.sin(tonePhase * 2 * Math.PI) * gain;
        for (let c = 0; c < channelCount; c++) buf[i * channelCount + c] = s;
        tonePhase += freqHz / sr;
        if (tonePhase >= 1) tonePhase -= 1;
      }
      output.writeInterleaved(buf, sr);
    }

    // Prefill ~100ms to avoid startup underruns.
    writeTone(Math.floor(sr / 10));

    toneTimer = window.setInterval(() => {
      const target = Math.floor(sr / 5); // ~200ms buffered
      const level = output.getBufferLevelFrames();
      const need = Math.max(0, target - level);
      if (need > 0) writeTone(need);

      status.textContent =
        `AudioContext: ${output.context.state}\n` +
        `sampleRate: ${sr}\n` +
        `bufferLevelFrames: ${output.getBufferLevelFrames()}\n` +
        `underruns: ${output.getUnderrunCount()}`;
    }, 50);
  }

  const button = el("button", {
    id: "init-audio-output",
    text: "Init audio output (test tone)",
    onclick: async () => {
      status.textContent = "";
      const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: "interactive" });
      // Expose for Playwright smoke tests.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioOutput = output;
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }
      try {
        startTone(output);
        await output.resume();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }
      status.textContent = "Audio initialized and test tone started.";
    },
  });

  return el("div", { class: "panel" }, el("h2", { text: "Audio" }), el("div", { class: "row" }, button), status);
}

function renderWorkersPanel(report: PlatformFeatureReport): HTMLElement {
  const support = workerCoordinator.checkSupport();

  const statusList = el("ul");
  const heartbeatLine = el("div", { class: "mono", text: "" });
  const error = el("pre", { text: "" });

  const guestRamSelect = el("select") as HTMLSelectElement;
  for (const mib of GUEST_RAM_PRESETS_MIB) {
    const label = mib % 1024 === 0 ? `${mib / 1024} GiB` : `${mib} MiB`;
    guestRamSelect.append(el("option", { value: String(mib), text: label }));
  }
  guestRamSelect.value = String(DEFAULT_GUEST_RAM_MIB);

  const startButton = el("button", {
    text: "Start workers",
    onclick: () => {
      error.textContent = "";
      try {
        workerCoordinator.start({ guestRamMiB: Number(guestRamSelect.value) as GuestRamMiB });
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const stopButton = el("button", {
    text: "Stop workers",
    onclick: () => {
      workerCoordinator.stop();
      update();
    },
  }) as HTMLButtonElement;

  const hint = el("div", {
    class: "mono",
    text: support.ok
      ? "Runs 4 module workers (cpu/gpu/io/jit). CPU increments a shared WebAssembly.Memory counter + emits ring-buffer heartbeats."
      : support.reason ?? "SharedArrayBuffer unavailable.",
  });

  function update(): void {
    const statuses = workerCoordinator.getWorkerStatuses();
    const anyActive = Object.values(statuses).some((s) => s.state !== "stopped");

    startButton.disabled = !support.ok || !report.wasmThreads || anyActive;
    stopButton.disabled = !anyActive;

    statusList.replaceChildren(
      ...Object.entries(statuses).map(([role, status]) => el("li", { text: `${role}: ${status.state}${status.error ? ` (${status.error})` : ""}` })),
    );

    heartbeatLine.textContent =
      `status[HeartbeatCounter]=${workerCoordinator.getHeartbeatCounter()}  ` +
      `ring[Heartbeat]=${workerCoordinator.getLastHeartbeatFromRing()}  ` +
      `guestI32[0]=${workerCoordinator.getGuestCounter0()}`;
  }

  update();
  globalThis.setInterval(update, 250);

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Workers" }),
    hint,
    el("div", { class: "row" }, el("label", { text: "Guest RAM:" }), guestRamSelect, startButton, stopButton),
    heartbeatLine,
    statusList,
    error,
  );
}

function renderIpcDemoPanel(): HTMLElement {
  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "IPC demo (SharedArrayBuffer ring buffers)" }),
    el(
      "p",
      {},
      "Open the high-rate command/event ring-buffer demo: ",
      el("a", { href: "/demo/ipc_demo.html" }, "/demo/ipc_demo.html"),
      ".",
    ),
  );
}

render();
