import "./style.css";

import { installAeroGlobals } from "./aero";
import { createGpuWorker } from "./main/createGpuWorker";
import { fnv1a32Hex } from "./utils/fnv1a";
import { createAudioOutput } from "./platform/audio";
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from "./platform/features";
import { importFileToOpfs } from "./platform/opfs";
import { RemoteStreamingDisk } from "./platform/remote_disk";
import { ensurePersistentStorage, getPersistentStorageInfo, getStorageEstimate } from "./platform/storage_quota";
import { requestWebGpuDevice } from "./platform/webgpu";
import { initAeroStatusApi } from "./api/status";
import { KeyboardCapture } from "./input/keyboard";
import { MouseCapture } from "./input/mouse";
import { installPerfHud } from "./perf/hud_entry";
import { HEADER_INDEX_FRAME_COUNTER, HEADER_INDEX_HEIGHT, HEADER_INDEX_WIDTH, wrapSharedFramebuffer } from "./display/framebuffer_protocol";
import { VgaPresenter } from "./display/vga_presenter";
import { installAeroGlobal } from "./runtime/aero_global";
import { WorkerCoordinator } from "./runtime/coordinator";
import { initWasm } from "./runtime/wasm_loader";
import { DEFAULT_GUEST_RAM_MIB, GUEST_RAM_PRESETS_MIB, type GuestRamMiB, type WorkerRole } from "./runtime/shared_layout";

initAeroStatusApi("booting");
installPerfHud({ guestRamBytes: DEFAULT_GUEST_RAM_MIB * 1024 * 1024 });
installAeroGlobal();

installAeroGlobals();

const workerCoordinator = new WorkerCoordinator();
const wasmInitPromise = initWasm();

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

function createExpectedTestPattern(width: number, height: number): Uint8Array {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const isLeft = x < halfW;
      const isTop = y < halfH;

      // Top-left origin:
      // - top-left: red
      // - top-right: green
      // - bottom-left: blue
      // - bottom-right: white
      let r = 0;
      let g = 0;
      let b = 0;
      if (isTop && isLeft) {
        r = 255;
      } else if (isTop && !isLeft) {
        g = 255;
      } else if (!isTop && isLeft) {
        b = 255;
      } else {
        r = 255;
        g = 255;
        b = 255;
      }

      out[i] = r;
      out[i + 1] = g;
      out[i + 2] = b;
      out[i + 3] = 255;
    }
  }

  return out;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return "unknown";
  const abs = Math.abs(bytes);
  if (abs < 1024) return `${bytes.toFixed(0)} B`;
  if (abs < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (abs < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (abs < 1024 * 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  return `${(bytes / (1024 * 1024 * 1024 * 1024)).toFixed(1)} TB`;
}

function formatMaybeBytes(bytes: number | null): string {
  return bytes === null ? "unknown" : formatBytes(bytes);
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
    renderWasmPanel(),
    renderGraphicsPanel(report),
    renderWebGpuPanel(),
    renderGpuWorkerPanel(),
    renderOpfsPanel(),
    renderRemoteDiskPanel(),
    renderAudioPanel(),
    renderInputPanel(),
    renderWorkersPanel(report),
    renderIpcDemoPanel(),
    renderMicrobenchPanel(),
  );
}

function renderCapabilityTable(report: PlatformFeatureReport): HTMLTableElement {
  const orderedKeys: Array<keyof PlatformFeatureReport> = [
    "crossOriginIsolated",
    "sharedArrayBuffer",
    "wasmSimd",
    "wasmThreads",
    "jit_dynamic_wasm",
    "webgpu",
    "webgl2",
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

function renderWasmPanel(): HTMLElement {
  const status = el("pre", { text: "Loading WASM…" });
  const output = el("pre", { text: "" });
  const error = el("pre", { text: "" });

  wasmInitPromise
    .then(({ api, variant, reason }) => {
      status.textContent = `Loaded variant: ${variant}\nReason: ${reason}`;
      output.textContent = `greet(\"Aero\") → ${api.greet("Aero")}\nadd(2, 3) → ${api.add(2, 3)}`;
      // Expose for quick interactive debugging / Playwright assertions.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroWasmApi = api;
    })
    .catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      status.textContent = "Failed to initialize WASM";
      error.textContent = message;
      console.error(err);
    });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "WASM runtime (threaded/single auto-selection)" }),
    status,
    output,
    error,
  );
}

function renderGraphicsPanel(report: PlatformFeatureReport): HTMLElement {
  const selected =
    report.webgpu ? "WebGPU" : report.webgl2 ? "WebGL2 (fallback)" : "Unavailable (no WebGPU/WebGL2)";

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Graphics backend" }),
    el("div", { class: "row" }, el("strong", { text: `Auto selection: ${selected}` })),
    el(
      "div",
      {},
      "Open the standalone fallback demo: ",
      el("a", { href: "/webgl2_fallback_demo.html" }, "/webgl2_fallback_demo.html"),
      ".",
    ),
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

function renderGpuWorkerPanel(): HTMLElement {
  const output = el("pre", { text: "" });
  const canvas = el("canvas") as HTMLCanvasElement;

  const cssWidth = 64;
  const cssHeight = 64;
  const devicePixelRatio = window.devicePixelRatio || 1;

  canvas.width = Math.max(1, Math.round(cssWidth * devicePixelRatio));
  canvas.height = Math.max(1, Math.round(cssHeight * devicePixelRatio));
  canvas.style.width = `${cssWidth}px`;
  canvas.style.height = `${cssHeight}px`;
  canvas.style.border = "1px solid #333";
  canvas.style.imageRendering = "pixelated";

  function appendLog(line: string): void {
    output.textContent += `${line}\n`;
  }

  let gpu: ReturnType<typeof createGpuWorker> | null = null;

  const button = el("button", {
    text: "Run GPU worker smoke test",
    onclick: async () => {
      output.textContent = "";

      try {
        if (!gpu) {
          gpu = createGpuWorker({
            canvas,
            width: cssWidth,
            height: cssHeight,
            devicePixelRatio,
            gpuOptions: {
              preferWebGpu: true,
            },
            onGpuError: (msg) => {
              appendLog(`gpu_error fatal=${msg.fatal} kind=${msg.error.kind} msg=${msg.error.message}`);
              if (msg.error.hints?.length) {
                for (const hint of msg.error.hints) appendLog(`  hint: ${hint}`);
              }
            },
          });
        }

        const ready = await gpu.ready;
        appendLog(`ready backend=${ready.backendKind}`);
        if (ready.fallback) {
          appendLog(`fallback ${ready.fallback.from} -> ${ready.fallback.to}: ${ready.fallback.reason}`);
        }
        if (ready.adapterInfo?.vendor || ready.adapterInfo?.renderer) {
          appendLog(`adapter vendor=${ready.adapterInfo.vendor ?? "n/a"} renderer=${ready.adapterInfo.renderer ?? "n/a"}`);
        }

        gpu.presentTestPattern();
        const screenshot = await gpu.requestScreenshot();

        const actual = new Uint8Array(screenshot.rgba8);
        const expected = createExpectedTestPattern(screenshot.width, screenshot.height);

        const actualHash = fnv1a32Hex(actual);
        const expectedHash = fnv1a32Hex(expected);

        appendLog(`screenshot ${screenshot.width}x${screenshot.height} rgba8 bytes=${actual.byteLength}`);
        appendLog(`hash actual=${actualHash} expected=${expectedHash}`);
        appendLog(actualHash === expectedHash ? "PASS" : "FAIL");
      } catch (err) {
        appendLog(err instanceof Error ? err.message : String(err));
      }
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "GPU Worker" }),
    el("div", { class: "row" }, button, canvas),
    output,
  );
}

function renderOpfsPanel(): HTMLElement {
  const quotaLine = el("div", { class: "mono", text: "Storage quota: loading…" });
  const persistenceLine = el("div", { class: "mono", text: "Persistent storage: loading…" });
  const persistenceResult = el("div", { class: "mono", text: "" });

  const refreshButton = el("button", {
    text: "Refresh storage info",
    onclick: async () => {
      persistenceResult.textContent = "";
      await refreshStorageInfo();
    },
  });

  const requestPersistenceButton = el("button", {
    text: "Request persistent storage",
    onclick: async () => {
      persistenceResult.textContent = "";
      const result = await ensurePersistentStorage();
      if (!result.supported) {
        persistenceResult.textContent = "Persistent storage request is not supported in this browser.";
      } else if (result.granted) {
        persistenceResult.textContent = "Persistent storage granted.";
      } else {
        persistenceResult.textContent = "Persistent storage not granted (denied or unavailable).";
      }
      await refreshStorageInfo();
    },
  });

  const status = el("pre", { text: "" });
  const progress = el("progress", { value: "0", max: "1", style: "width: 320px" }) as HTMLProgressElement;
  const destPathInput = el("input", { type: "text", value: "images/disk.img" }) as HTMLInputElement;
  const fileInput = el("input", { type: "file" }) as HTMLInputElement;

  async function refreshStorageInfo(): Promise<void> {
    const estimate = await getStorageEstimate();
    if (!estimate.supported) {
      quotaLine.className = "mono";
      quotaLine.textContent = "Storage quota: unsupported in this browser.";
    } else if (
      estimate.usageBytes === null ||
      estimate.quotaBytes === null ||
      estimate.usagePercent === null ||
      estimate.remainingBytes === null
    ) {
      quotaLine.className = "mono";
      quotaLine.textContent = "Storage quota: unavailable.";
    } else {
      quotaLine.className = estimate.warning ? "mono bad" : "mono";
      const percent = estimate.usagePercent.toFixed(1);
      quotaLine.textContent = `Storage quota: ${formatBytes(estimate.usageBytes)} used / ${formatBytes(
        estimate.quotaBytes,
      )} quota (${percent}% used, ${formatBytes(estimate.remainingBytes)} free)${estimate.warning ? " (warning: >80%)" : ""}`;
    }

    const persistence = await getPersistentStorageInfo();
    if (!persistence.supported) {
      persistenceLine.className = "mono";
      persistenceLine.textContent = "Persistent storage: unsupported in this browser.";
      return;
    }
    if (persistence.persisted === null) {
      persistenceLine.className = "mono";
      persistenceLine.textContent = "Persistent storage: unknown.";
      return;
    }
    persistenceLine.className = persistence.persisted ? "mono ok" : "mono bad";
    persistenceLine.textContent = persistence.persisted ? "Persistent storage: granted." : "Persistent storage: not granted.";
  }

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
        const estimate = await getStorageEstimate();
        if (estimate.supported && estimate.remainingBytes !== null) {
          // OPFS metadata + internal fragmentation can require extra headroom.
          const safetyMarginBytes = Math.max(50 * 1024 * 1024, Math.floor(file.size * 0.05));
          const requiredBytes = file.size + safetyMarginBytes;

          if (estimate.remainingBytes < requiredBytes) {
            const ok = window.confirm(
              `Estimated remaining browser storage (${formatMaybeBytes(estimate.remainingBytes)}) is less than the recommended free space (${formatBytes(
                requiredBytes,
              )}) for this import.\n\nThe import may fail or the browser may evict data.\n\nContinue anyway?`,
            );
            if (!ok) return;
          }
        }

        await importFileToOpfs(file, destPath, ({ writtenBytes, totalBytes }) => {
          progress.value = totalBytes ? writtenBytes / totalBytes : 0;
          status.textContent = `Writing ${writtenBytes.toLocaleString()} / ${totalBytes.toLocaleString()} bytes…`;
        });
        status.textContent = `Imported to OPFS: ${destPath}`;
        await refreshStorageInfo();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  fileInput.addEventListener("change", () => {
    const file = fileInput.files?.[0];
    if (file) destPathInput.value = `images/${file.name}`;
  });

  const panel = el(
    "div",
    { class: "panel" },
    el("h2", { text: "OPFS (disk image import)" }),
    el("h3", { text: "Quota & durability" }),
    el("div", { class: "row" }, refreshButton, requestPersistenceButton),
    quotaLine,
    persistenceLine,
    persistenceResult,
    el("h3", { text: "Import" }),
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

  refreshStorageInfo().catch((err) => {
    quotaLine.className = "mono bad";
    quotaLine.textContent = `Storage quota: error (${err instanceof Error ? err.message : String(err)})`;
    persistenceLine.className = "mono bad";
    persistenceLine.textContent = "Persistent storage: error.";
  });

  return panel;
}

function renderRemoteDiskPanel(): HTMLElement {
  const warning = el(
    "div",
    { class: "mono" },
    "Remote disk images are experimental. Only use images you own/have rights to. ",
    "The server must support HTTP Range requests and CORS (see docs/disk-images.md). ",
    "For local testing, run `docker compose up` in `infra/local-object-store/` and upload an object to `disk-images/`.",
  );

  const enabledInput = el("input", { type: "checkbox" }) as HTMLInputElement;
  const urlInput = el("input", { type: "url", placeholder: "http://localhost:9002/disk-images/large.bin" }) as HTMLInputElement;
  const blockSizeInput = el("input", { type: "number", value: String(1024), min: "4" }) as HTMLInputElement;
  const cacheLimitInput = el("input", { type: "number", value: String(512), min: "0" }) as HTMLInputElement;
  const output = el("pre", { text: "" });

  const probeButton = el("button", { text: "Probe Range support" }) as HTMLButtonElement;
  const readButton = el("button", { text: "Read sample bytes" }) as HTMLButtonElement;
  const progress = el("progress", { value: "0", max: "1", style: "width: 320px" }) as HTMLProgressElement;

  let disk: RemoteStreamingDisk | null = null;

  function updateButtons(): void {
    const enabled = enabledInput.checked;
    probeButton.disabled = !enabled;
    readButton.disabled = !enabled;
  }

  enabledInput.addEventListener("change", updateButtons);
  updateButtons();

  probeButton.onclick = async () => {
    output.textContent = "";
    progress.value = 0;
    const url = urlInput.value.trim();
    if (!url) {
      output.textContent = "Enter a URL first.";
      return;
    }

    try {
      const blockSize = Number(blockSizeInput.value) * 1024;
      const cacheLimitMiB = Number(cacheLimitInput.value);
      const cacheLimitBytes = cacheLimitMiB <= 0 ? null : cacheLimitMiB * 1024 * 1024;

      output.textContent = "Probing… (this will make HTTP requests)\n";
      disk = await RemoteStreamingDisk.open(url, {
        blockSize,
        cacheLimitBytes,
        prefetchSequentialBlocks: 2,
      });
      const status = await disk.getCacheStatus();
      output.textContent = JSON.stringify(status, null, 2);
    } catch (err) {
      output.textContent = err instanceof Error ? err.message : String(err);
    }
  };

  readButton.onclick = async () => {
    output.textContent = "";
    progress.value = 0;
    const url = urlInput.value.trim();
    if (!url) {
      output.textContent = "Enter a URL first.";
      return;
    }

    try {
      if (!disk) {
        const blockSize = Number(blockSizeInput.value) * 1024;
        const cacheLimitMiB = Number(cacheLimitInput.value);
        const cacheLimitBytes = cacheLimitMiB <= 0 ? null : cacheLimitMiB * 1024 * 1024;
        disk = await RemoteStreamingDisk.open(url, { blockSize, cacheLimitBytes, prefetchSequentialBlocks: 2 });
      }

      const logLines: string[] = [];
      const bytes = await disk.read(1024, 16, (msg) => {
        logLines.push(msg);
        output.textContent = logLines.join("\n");
      });

      const status = await disk.getCacheStatus();
      output.textContent = JSON.stringify(
        { read: { offset: 1024, length: 16, bytes: Array.from(bytes) }, cache: status, log: logLines },
        null,
        2,
      );
      progress.value = 1;
    } catch (err) {
      output.textContent = err instanceof Error ? err.message : String(err);
    }
  };

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Remote disk image (streaming via HTTP Range)" }),
    warning,
    el(
      "div",
      { class: "row" },
      el("label", { text: "Enable:" }),
      enabledInput,
      el("label", { text: "URL:" }),
      urlInput,
    ),
    el(
      "div",
      { class: "row" },
      el("label", { text: "Block KiB:" }),
      blockSizeInput,
      el("label", { text: "Cache MiB (0=off):" }),
      cacheLimitInput,
      probeButton,
      readButton,
      progress,
    ),
    output,
  );
}

function renderAudioPanel(): HTMLElement {
  const status = el("pre", { text: "" });
  let toneTimer: number | null = null;
  let tonePhase = 0;
  let wasmBridge: unknown | null = null;
  let wasmTone: { free(): void } | null = null;

  function stopTone() {
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
    if (wasmTone) {
      wasmTone.free();
      wasmTone = null;
    }
    if (wasmBridge && typeof (wasmBridge as { free?: () => void }).free === "function") {
      (wasmBridge as { free(): void }).free();
      wasmBridge = null;
    }
  }

  async function startTone(output: Exclude<Awaited<ReturnType<typeof createAudioOutput>>, { enabled: false }>) {
    stopTone();

    const freqHz = 440;
    const gain = 0.1;
    const channelCount = output.ringBuffer.channelCount;
    const sr = output.context.sampleRate;

    // Try to use the WASM-side bridge + sine generator if available; fall back to JS.
    let writeTone: (frames: number) => void;
    try {
      const { api } = await wasmInitPromise;
      if (
        typeof api.attach_worklet_bridge === "function" &&
        typeof api.SineTone === "function" &&
        output.ringBuffer.buffer instanceof SharedArrayBuffer
      ) {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        wasmBridge = (api.attach_worklet_bridge as any)(output.ringBuffer.buffer, output.ringBuffer.capacityFrames, channelCount);
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        wasmTone = new (api.SineTone as any)() as { free(): void; write: (...args: unknown[]) => number };

        writeTone = (frames: number) => {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (wasmTone as any).write(wasmBridge, frames, freqHz, sr, gain);
        };

        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (globalThis as any).__aeroAudioToneBackend = "wasm";
      } else {
        throw new Error("WASM audio bridge not available");
      }
    } catch {
      writeTone = (frames: number) => {
        const buf = new Float32Array(frames * channelCount);
        for (let i = 0; i < frames; i++) {
          const s = Math.sin(tonePhase * 2 * Math.PI) * gain;
          for (let c = 0; c < channelCount; c++) buf[i * channelCount + c] = s;
          tonePhase += freqHz / sr;
          if (tonePhase >= 1) tonePhase -= 1;
        }
        output.writeInterleaved(buf, sr);
      };

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioToneBackend = "js";
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
        await startTone(output);
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

function renderMicrobenchPanel(): HTMLElement {
  const output = el("pre", { text: "" });

  const runButton = el("button", {
    text: "Run microbench suite",
    onclick: async () => {
      output.textContent = "";
      runButton.disabled = true;
      try {
        if (!window.aero?.bench?.runMicrobenchSuite) {
          output.textContent = "window.aero.bench.runMicrobenchSuite is not available.";
          return;
        }
        const results = await window.aero.bench.runMicrobenchSuite();
        output.textContent = JSON.stringify(results, null, 2);
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        runButton.disabled = false;
      }
    },
  }) as HTMLButtonElement;

  const exportButton = el("button", {
    text: "Export perf JSON",
    onclick: () => {
      output.textContent = "";
      try {
        if (!window.aero?.perf?.export) {
          output.textContent = "window.aero.perf.export is not available.";
          return;
        }
        output.textContent = JSON.stringify(window.aero.perf.export(), null, 2);
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  }) as HTMLButtonElement;

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Microbench" }),
    el(
      "div",
      { class: "row" },
      runButton,
      exportButton,
      el("span", { class: "mono", text: "Runs deterministic WASM microbench hot paths (ALU, branch, memcpy, hash)." }),
    ),
    output,
  );
}

function renderInputPanel(): HTMLElement {
  const log = el("pre", { text: "" });
  const canvas = el("canvas", {
    width: "640",
    height: "360",
    tabindex: "0",
    style: "border: 1px solid #444; background: #111; width: 640px; height: 360px;",
  }) as HTMLCanvasElement;

  const append = (line: string): void => {
    log.textContent = `${log.textContent ?? ""}${line}\n`;
    log.scrollTop = log.scrollHeight;
  };

  const keyboard = new KeyboardCapture(canvas, (bytes) => {
    append(`kbd: ${bytes.map((b) => b.toString(16).padStart(2, "0")).join(" ")}`);
  });
  keyboard.attach();

  const mouse = new MouseCapture(
    canvas,
    (dx, dy, wheel) => {
      if (wheel !== 0) {
        append(`mouse: wheel=${wheel}`);
        return;
      }
      if (dx !== 0 || dy !== 0) {
        append(`mouse: dx=${dx} dy=${dy}`);
      }
    },
    (button, pressed) => {
      append(`mouse: button=${button} ${pressed ? "down" : "up"}`);
    },
  );
  mouse.attach();

  const hint = el("div", {
    class: "mono",
    text: "Click the canvas to focus + request pointer lock. Keyboard events are captured via KeyboardEvent.code.",
  });

  const clear = el("button", {
    text: "Clear log",
    onclick: () => {
      log.textContent = "";
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Input capture (PS/2 Set-2 scancodes)" }),
    hint,
    el("div", { class: "row" }, clear),
    canvas,
    log,
  );
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

  const vgaCanvas = el("canvas") as HTMLCanvasElement;
  vgaCanvas.style.width = "640px";
  vgaCanvas.style.height = "480px";
  vgaCanvas.style.border = "1px solid #333";
  vgaCanvas.style.background = "#000";

  const vgaInfoLine = el("div", { class: "mono", text: "" });

  let vgaPresenter: VgaPresenter | null = null;
  let vgaShared: ReturnType<typeof wrapSharedFramebuffer> | null = null;
  let vgaSab: SharedArrayBuffer | null = null;

  function ensureVgaPresenter(): void {
    const sab = workerCoordinator.getVgaFramebuffer();
    if (!sab) return;

    if (sab !== vgaSab) {
      vgaSab = sab;
      vgaShared = wrapSharedFramebuffer(sab, 0);
      if (vgaPresenter) {
        vgaPresenter.setSharedFramebuffer(vgaShared);
      }
    }

    if (!vgaPresenter && vgaShared) {
      vgaPresenter = new VgaPresenter(vgaCanvas, { scaleMode: "auto", integerScaling: true, maxPresentHz: 60 });
      vgaPresenter.setSharedFramebuffer(vgaShared);
      vgaPresenter.start();
    }
  }

  function teardownVgaPresenter(): void {
    if (vgaPresenter) {
      vgaPresenter.destroy();
      vgaPresenter = null;
    }
    vgaShared = null;
    vgaSab = null;
    vgaInfoLine.textContent = "";
  }

  function update(): void {
    const statuses = workerCoordinator.getWorkerStatuses();
    const anyActive = Object.values(statuses).some((s) => s.state !== "stopped");

    startButton.disabled = !support.ok || !report.wasmThreads || anyActive;
    stopButton.disabled = !anyActive;

    statusList.replaceChildren(
      ...Object.entries(statuses).map(([role, status]) => {
        const roleName = role as WorkerRole;
        const wasm = workerCoordinator.getWorkerWasmStatus(roleName);
        const wasmSuffix =
          roleName === "cpu" && status.state !== "stopped"
            ? wasm
              ? ` wasm(${wasm.variant}) version=${wasm.version} sum=${wasm.sum}`
              : " wasm(pending)"
            : "";
        return el("li", {
          text: `${roleName}: ${status.state}${status.error ? ` (${status.error})` : ""}${wasmSuffix}`,
        });
      }),
    );

    heartbeatLine.textContent =
      `status[HeartbeatCounter]=${workerCoordinator.getHeartbeatCounter()}  ` +
      `ring[Heartbeat]=${workerCoordinator.getLastHeartbeatFromRing()}  ` +
      `guestI32[0]=${workerCoordinator.getGuestCounter0()}`;

    if (anyActive) {
      ensureVgaPresenter();
      if (vgaShared) {
        const w = Atomics.load(vgaShared.header, HEADER_INDEX_WIDTH);
        const h = Atomics.load(vgaShared.header, HEADER_INDEX_HEIGHT);
        const frame = Atomics.load(vgaShared.header, HEADER_INDEX_FRAME_COUNTER);
        vgaInfoLine.textContent = `vga ${w}x${h} frame=${frame}`;
      }
    } else {
      teardownVgaPresenter();
    }
  }

  update();
  globalThis.setInterval(update, 250);

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Workers" }),
    hint,
    el("div", { class: "row" }, el("label", { text: "Guest RAM:" }), guestRamSelect, startButton, stopButton),
    el("div", { class: "row" }, vgaCanvas),
    vgaInfoLine,
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
