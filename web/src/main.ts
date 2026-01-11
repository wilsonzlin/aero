import "./style.css";

import { installAeroGlobals } from "./aero";
import { startFrameScheduler, type FrameSchedulerHandle } from "./main/frameScheduler";
import { GpuRuntime } from "./gpu/gpuRuntime";
import { fnv1a32Hex } from "./utils/fnv1a";
import { perf } from "./perf/perf";
import { createAdaptiveRingBufferTarget, createAudioOutput, startAudioPerfSampling } from "./platform/audio";
import { MicCapture } from "./audio/mic_capture";
import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from "./platform/features";
import { importFileToOpfs, openFileHandle, removeOpfsEntry } from "./platform/opfs";
import { ensurePersistentStorage, getPersistentStorageInfo, getStorageEstimate } from "./platform/storage_quota";
import { mountWebHidPassthroughPanel, WebHidPassthroughManager } from "./platform/webhid_passthrough";
import { initAeroStatusApi } from "./api/status";
import { AeroConfigManager } from "./config/manager";
import { InputCapture } from "./input/input_capture";
import { InputEventType, type InputBatchTarget } from "./input/event_queue";
import { installPerfHud } from "./perf/hud_entry";
import { HEADER_INDEX_FRAME_COUNTER, HEADER_INDEX_HEIGHT, HEADER_INDEX_WIDTH, wrapSharedFramebuffer } from "./display/framebuffer_protocol";
import { VgaPresenter } from "./display/vga_presenter";
import { installAeroGlobal } from "./runtime/aero_global";
import { createWebGpuCanvasContext, requestWebGpuDevice } from "./platform/webgpu";
import { WorkerCoordinator } from "./runtime/coordinator";
import { initWasm, type WasmApi, type WasmVariant } from "./runtime/wasm_loader";
import { precompileWasm } from "./runtime/wasm_preload";
import type { WorkerRole } from "./runtime/shared_layout";
import { createDefaultDiskImageStore } from "./storage/default_disk_image_store";
import type { DiskImageInfo, WorkerOpenToken } from "./storage/disk_image_store";
import { formatByteSize } from "./storage/disk_image_store";
import { RuntimeDiskClient } from "./storage/runtime_disk_client";
import { IoWorkerClient } from "./workers/io_worker_client";
import { type JitWorkerResponse } from "./workers/jit_protocol";
import { JitWorkerClient } from "./workers/jit_worker_client";
import { FRAME_SEQ_INDEX, FRAME_STATUS_INDEX } from "./shared/frameProtocol";
import { mountSettingsPanel } from "./ui/settings_panel";
import { mountStatusPanel } from "./ui/status_panel";
import { renderWebUsbPanel } from "./usb/webusb_panel";

const configManager = new AeroConfigManager({ staticConfigUrl: "/aero.config.json" });
void configManager.init();

initAeroStatusApi("booting");
installPerfHud({ guestRamBytes: configManager.getState().effective.guestMemoryMiB * 1024 * 1024 });
installAeroGlobal();
perf.installGlobalApi();

if (new URLSearchParams(location.search).has("trace")) perf.traceStart();
perf.instant("boot:main:start", "p");

installAeroGlobals();

const workerCoordinator = new WorkerCoordinator();
configManager.subscribe((state) => {
  workerCoordinator.updateConfig(state.effective);
});
const wasmInitPromise = perf.spanAsync("wasm:init", async () => {
  const preferThreaded = (() => {
    if (!(globalThis as any).crossOriginIsolated) return false;
    if (typeof SharedArrayBuffer === "undefined") return false;
    if (typeof Atomics === "undefined") return false;
    if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory !== "function") return false;
    try {
      // eslint-disable-next-line no-new
      new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      return true;
    } catch {
      return false;
    }
  })();

  const preferredVariant: WasmVariant = preferThreaded ? "threaded" : "single";
  try {
    const { module } = await precompileWasm(preferredVariant);
    return await initWasm({ variant: preferredVariant, module });
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    console.warn(`[wasm] Precompile (${preferredVariant}) failed; falling back to default init. Error: ${message}`);
    return await initWasm();
  }
});
let frameScheduler: FrameSchedulerHandle | null = null;

// Updated by the microphone UI and read by the worker coordinator so that
// newly-started workers inherit the current mic attachment (if any).
let micAttachment: { ringBuffer: SharedArrayBuffer; sampleRate: number } | null = null;

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

function renderBuildInfoPanel(): HTMLElement {
  const versionLink = el("a", {
    href: "/aero.version.json",
    target: "_blank",
    rel: "noreferrer",
    text: "aero.version.json",
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Build info" }),
    el("div", { class: "row" }, el("strong", { text: "Version:" }), el("span", { class: "mono", text: __AERO_BUILD_INFO__.version })),
    el("div", { class: "row" }, el("strong", { text: "Commit:" }), el("span", { class: "mono", text: __AERO_BUILD_INFO__.gitSha })),
    el(
      "div",
      { class: "row" },
      el("strong", { text: "Built:" }),
      el("span", { class: "mono", text: __AERO_BUILD_INFO__.builtAt }),
    ),
    el("div", { class: "hint muted" }, "Also available at ", versionLink, "."),
  );
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

const ACTIVE_DISK_KEY = "aero.activeDiskImage";

function getActiveDiskName(): string | null {
  return localStorage.getItem(ACTIVE_DISK_KEY);
}

function setActiveDiskName(name: string | null): void {
  if (name === null) {
    localStorage.removeItem(ACTIVE_DISK_KEY);
    return;
  }
  localStorage.setItem(ACTIVE_DISK_KEY, name);
}

function render(): void {
  const app = document.getElementById("app");
  if (!app) throw new Error("Missing #app element");

  const report = detectPlatformFeatures();
  const missing = explainMissingRequirements(report);

  const settingsHost = el("div", { class: "panel" });
  mountSettingsPanel(settingsHost, configManager);

  const statusHost = el("div", { class: "panel" });
  mountStatusPanel(statusHost, configManager, workerCoordinator);

  app.replaceChildren(
    el("h1", { text: "Aero Platform Capabilities" }),
    renderBuildInfoPanel(),
    settingsHost,
    statusHost,
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
    renderSnapshotPanel(),
    renderWebGpuPanel(),
    renderGpuWorkerPanel(),
    renderSm5TrianglePanel(),
    renderOpfsPanel(),
    renderDiskImagesPanel(),
    renderRemoteDiskPanel(),
    renderAudioPanel(),
    renderMicrophonePanel(),
    renderWebHidPassthroughPanel(),
    renderInputPanel(),
    renderWebUsbPanel(report),
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
    "webusb",
    "webgl2",
    "opfs",
    "opfsSyncAccessHandle",
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

async function writeSnapshotToOpfs(path: string, bytes: Uint8Array): Promise<void> {
  const handle = await openFileHandle(path, { create: true });
  const writable = await handle.createWritable({ keepExistingData: false });
  await writable.write(bytes);
  await writable.close();
}

async function readSnapshotFromOpfs(path: string): Promise<Uint8Array | null> {
  try {
    const handle = await openFileHandle(path, { create: false });
    const file = await handle.getFile();
    return new Uint8Array(await file.arrayBuffer());
  } catch (err) {
    if (err instanceof DOMException && err.name === "NotFoundError") return null;
    throw err;
  }
}

function downloadBytes(bytes: Uint8Array, filename: string): void {
  const blob = new Blob([bytes], { type: "application/octet-stream" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

function renderSnapshotPanel(): HTMLElement {
  const status = el("pre", { text: "Initializing demo VM…" });
  const output = el("pre", { text: "" });
  const error = el("pre", { text: "" });

  const autosaveInput = el("input", { type: "number", min: "0", step: "1", value: "0" }) as HTMLInputElement;
  const importInput = el("input", { type: "file", accept: ".snap,application/octet-stream" }) as HTMLInputElement;

  const saveButton = el("button", { text: "Save", disabled: "true" }) as HTMLButtonElement;
  const loadButton = el("button", { text: "Load", disabled: "true" }) as HTMLButtonElement;
  const exportButton = el("button", { text: "Export", disabled: "true" }) as HTMLButtonElement;
  const deleteButton = el("button", { text: "Delete", disabled: "true" }) as HTMLButtonElement;

  const SNAPSHOT_PATH = "state/demo-vm-autosave.snap";

  let autosaveTimer: number | null = null;
  let vm: InstanceType<WasmApi["DemoVm"]> | null = null;

  let steps = 0;

  function setError(msg: string): void {
    error.textContent = msg;
    console.error(msg);
  }

  async function saveSnapshot(): Promise<Uint8Array> {
    if (!vm) throw new Error("Demo VM not initialized");
    const bytes = vm.snapshot_full();
    await writeSnapshotToOpfs(SNAPSHOT_PATH, bytes);
    status.textContent = `Saved snapshot (${bytes.byteLength.toLocaleString()} bytes)`;
    return bytes;
  }

  async function loadSnapshot(bytesOverride?: Uint8Array): Promise<void> {
    if (!vm) throw new Error("Demo VM not initialized");
    const bytes = bytesOverride ?? (await readSnapshotFromOpfs(SNAPSHOT_PATH));
    if (!bytes) {
      status.textContent = "No snapshot found in OPFS.";
      return;
    }
    vm.restore_snapshot(bytes);
    status.textContent = `Loaded snapshot (${bytes.byteLength.toLocaleString()} bytes)`;
  }

  function setAutosave(seconds: number): void {
    if (autosaveTimer !== null) {
      window.clearInterval(autosaveTimer);
      autosaveTimer = null;
    }
    if (!Number.isFinite(seconds) || seconds <= 0) {
      status.textContent = "Auto-save disabled.";
      return;
    }
    autosaveTimer = window.setInterval(() => {
      saveSnapshot().catch((err) => setError(err instanceof Error ? err.message : String(err)));
    }, seconds * 1000);
    status.textContent = `Auto-save every ${seconds}s.`;
  }

  autosaveInput.addEventListener("change", () => {
    const seconds = Number.parseInt(autosaveInput.value, 10);
    setAutosave(seconds);
  });

  saveButton.onclick = () => {
    error.textContent = "";
    saveSnapshot().catch((err) => setError(err instanceof Error ? err.message : String(err)));
  };

  loadButton.onclick = () => {
    error.textContent = "";
    loadSnapshot().catch((err) => setError(err instanceof Error ? err.message : String(err)));
  };

  exportButton.onclick = () => {
    error.textContent = "";
    readSnapshotFromOpfs(SNAPSHOT_PATH)
      .then((bytes) => {
        if (!bytes) {
          status.textContent = "No snapshot found to export.";
          return;
        }
        downloadBytes(bytes, "aero-demo-vm.snap");
        status.textContent = `Exported snapshot (${bytes.byteLength.toLocaleString()} bytes)`;
      })
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  };

  deleteButton.onclick = () => {
    error.textContent = "";
    removeOpfsEntry(SNAPSHOT_PATH)
      .then(() => {
        status.textContent = "Deleted snapshot from OPFS.";
      })
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  };

  importInput.addEventListener("change", () => {
    void (async () => {
      error.textContent = "";
      const file = importInput.files?.[0];
      if (!file) return;
      const bytes = new Uint8Array(await file.arrayBuffer());
      await loadSnapshot(bytes);
      await writeSnapshotToOpfs(SNAPSHOT_PATH, bytes);
      status.textContent = `Imported snapshot (${bytes.byteLength.toLocaleString()} bytes)`;
    })().catch((err) => setError(err instanceof Error ? err.message : String(err)));
  });

  wasmInitPromise
    .then(async ({ api, variant }) => {
      vm = new api.DemoVm(256 * 1024);
      status.textContent = `Demo VM ready (WASM ${variant}). Running…`;

      saveButton.disabled = false;
      loadButton.disabled = false;
      exportButton.disabled = false;
      deleteButton.disabled = false;

      // Best-effort crash recovery: try to restore the last autosave snapshot.
      try {
        await loadSnapshot();
      } catch (err) {
        // If restore fails, keep running from a clean state.
        setError(err instanceof Error ? err.message : String(err));
      }

      // Drive the demo VM forward so the snapshot has something interesting to capture.
      window.setInterval(() => {
        if (!vm) return;
        vm.run_steps(5_000);
        steps += 5_000;
        const outBytes = vm.serial_output();
        output.textContent = `steps=${steps.toLocaleString()} serial_bytes=${outBytes.byteLength.toLocaleString()}`;
      }, 250);
    })
    .catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      status.textContent = "Demo VM unavailable (WASM init failed)";
      setError(message);
      console.error(err);
    });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Snapshots (demo VM + OPFS autosave)" }),
    el(
      "div",
      { class: "row" },
      saveButton,
      loadButton,
      exportButton,
      deleteButton,
      el("label", { text: "Auto-save (seconds):" }),
      autosaveInput,
      el("label", { text: "Import:" }),
      importInput,
    ),
    status,
    output,
    error,
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

  let runtime: GpuRuntime | null = null;

  const button = el("button", {
    text: "Run GPU runtime smoke test",
    onclick: async () => {
      output.textContent = "";

      try {
        if (!runtime) {
          runtime = new GpuRuntime();
          await runtime.init(canvas, cssWidth, cssHeight, devicePixelRatio, {
            mode: "auto",
            gpuOptions: { preferWebGpu: true },
            onError: (msg) => {
              appendLog(`gpu error: ${msg.message}${msg.code ? ` (code=${msg.code})` : ""}`);
            },
          });
          if (runtime.workerReady) {
            const ready = runtime.workerReady;
            appendLog(`ready backend=${ready.backendKind}`);
            if (ready.fallback) {
              appendLog(`fallback ${ready.fallback.from} -> ${ready.fallback.to}: ${ready.fallback.reason}`);
            }
          } else {
            appendLog(`ready backend=${runtime.backendKind ?? "webgl2"} (main-thread)`);
          }

          appendLog(`runtime mode=${runtime.mode} backend=${runtime.backendKind ?? "n/a"}`);
        }

        await runtime.present();
        const screenshot = await runtime.screenshot();

        const actual = new Uint8Array(screenshot.data.buffer, screenshot.data.byteOffset, screenshot.data.byteLength);
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
    el("h2", { text: "GPU Runtime" }),
    el("div", { class: "row" }, button, canvas),
    output,
  );
}

function renderSm5TrianglePanel(): HTMLElement {
  const output = el("pre", { text: "" });
  const canvas = el("canvas") as HTMLCanvasElement;

  const cssWidth = 320;
  const cssHeight = 320;
  const devicePixelRatio = window.devicePixelRatio || 1;

  canvas.width = Math.max(1, Math.round(cssWidth * devicePixelRatio));
  canvas.height = Math.max(1, Math.round(cssHeight * devicePixelRatio));
  canvas.style.width = `${cssWidth}px`;
  canvas.style.height = `${cssHeight}px`;
  canvas.style.border = "1px solid #2a2a2a";

  // WGSL below is intentionally kept in sync with the current output of the
  // `crates/aero-d3d11` bootstrap DXBC→WGSL translator for the synthetic SM5
  // passthrough shaders (mov o*, v*; ret).
  const vsWgsl = `
struct VsIn {
  @location(0) v0: vec4<f32>,
  @location(1) v1: vec4<f32>,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) o1: vec4<f32>,
};

@vertex
fn main(input: VsIn) -> VsOut {
  var out: VsOut;
  out.pos = input.v0;
  out.o1 = input.v1;
  return out;
}
`;

  const psWgsl = `
struct PsIn {
  @builtin(position) pos: vec4<f32>,
  @location(0) v1: vec4<f32>,
};

@fragment
fn main(input: PsIn) -> @location(0) vec4<f32> {
  return input.v1;
}
`;

  const button = el("button", {
    text: "Render SM5 passthrough triangle",
    onclick: async () => {
      output.textContent = "";
      try {
        const { device, preferredFormat } = await requestWebGpuDevice({ powerPreference: "high-performance" });
        const context = createWebGpuCanvasContext(canvas, device, preferredFormat);

        const vertexModule = device.createShaderModule({ code: vsWgsl });
        const fragmentModule = device.createShaderModule({ code: psWgsl });

        const pipeline = device.createRenderPipeline({
          layout: "auto",
          vertex: {
            module: vertexModule,
            entryPoint: "main",
            buffers: [
              {
                arrayStride: 32,
                attributes: [
                  { shaderLocation: 0, offset: 0, format: "float32x4" },
                  { shaderLocation: 1, offset: 16, format: "float32x4" },
                ],
              },
            ],
          },
          fragment: {
            module: fragmentModule,
            entryPoint: "main",
            targets: [{ format: preferredFormat }],
          },
          primitive: {
            topology: "triangle-list",
          },
        });

        const vertices = new Float32Array([
          // position (x,y,z,w), color (r,g,b,a)
          0.0, 0.7, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0,
          -0.7, -0.7, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0,
          0.7, -0.7, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0,
        ]);

        const vertexBuffer = device.createBuffer({
          size: vertices.byteLength,
          usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
        });
        device.queue.writeBuffer(vertexBuffer, 0, vertices);

        const encoder = device.createCommandEncoder();
        const pass = encoder.beginRenderPass({
          colorAttachments: [
            {
              view: context.getCurrentTexture().createView(),
              clearValue: { r: 0.06, g: 0.06, b: 0.08, a: 1.0 },
              loadOp: "clear",
              storeOp: "store",
            },
          ],
        });
        pass.setPipeline(pipeline);
        pass.setVertexBuffer(0, vertexBuffer);
        pass.draw(3, 1, 0, 0);
        pass.end();

        device.queue.submit([encoder.finish()]);
        output.textContent = "Rendered.";
      } catch (err) {
        output.textContent = err instanceof Error ? err.message : String(err);
      }
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "SM5 passthrough triangle (WebGPU)" }),
    el("div", { class: "row" }, button),
    canvas,
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
    el("h2", { text: "Disk Images" }),
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

function renderDiskImagesPanel(): HTMLElement {
  const { store, persistent, warning } = createDefaultDiskImageStore();
  const ioWorker = new IoWorkerClient();

  const warningEl = el("div", {
    class: "warning",
    text: warning ?? "OPFS unavailable; using in-memory disk image store.",
  });
  warningEl.hidden = persistent;

  const statusEl = el("span", { class: "muted", text: "" }) as HTMLSpanElement;

  const importNameInput = el("input", { type: "text", placeholder: "Defaults to file name" }) as HTMLInputElement;
  const fileInput = el("input", { type: "file", style: "display: none" }) as HTMLInputElement;

  const importProgress = el("progress", { value: "0", max: "1", style: "width: 320px" }) as HTMLProgressElement;
  importProgress.hidden = true;
  const importProgressText = el("span", { class: "muted", text: "" }) as HTMLSpanElement;

  const tableBody = el("tbody");

  const setProgress = (loaded: number, total: number) => {
    importProgress.hidden = false;
    importProgress.max = Math.max(total, 1);
    importProgress.value = loaded;
    const pct = total > 0 ? Math.floor((loaded / total) * 100) : 0;
    importProgressText.textContent = `${formatByteSize(loaded)} / ${formatByteSize(total)} (${pct}%)`;
  };

  const clearProgress = () => {
    importProgress.hidden = true;
    importProgress.value = 0;
    importProgress.max = 1;
    importProgressText.textContent = "";
  };

  const renderList = (images: DiskImageInfo[]) => {
    const active = getActiveDiskName();
    tableBody.replaceChildren();

    if (images.length === 0) {
      tableBody.append(el("tr", {}, el("td", { colspan: "4", class: "muted", text: "No disk images imported yet." })));
      return;
    }

    for (const image of images) {
      const radio = el("input", {
        type: "radio",
        name: "active-disk",
        onchange: () => {
          setActiveDiskName(image.name);
          void refreshList();
        },
      }) as HTMLInputElement;
      radio.checked = image.name === active;

      const exportButton = el("button", {
        text: "Export",
        onclick: async () => {
          const blob = await store.export(image.name);
          const url = URL.createObjectURL(blob);
          try {
            const a = document.createElement("a");
            a.href = url;
            a.download = image.name;
            a.click();
          } finally {
            setTimeout(() => URL.revokeObjectURL(url), 1000);
          }
        },
      });

      const deleteButton = el("button", {
        text: "Delete",
        onclick: async () => {
          if (!confirm(`Delete disk image "${image.name}"?`)) return;
          await store.delete(image.name);
          if (getActiveDiskName() === image.name) setActiveDiskName(null);
          await refreshList();
        },
      });

      tableBody.append(
        el(
          "tr",
          {},
          el("td", {}, radio),
          el("td", { text: image.name }),
          el("td", { text: formatByteSize(image.size) }),
          el("td", { class: "actions" }, exportButton, deleteButton),
        ),
      );
    }
  };

  const refreshList = async () => {
    try {
      const images = await store.list();
      renderList(images);
    } catch (err) {
      statusEl.textContent = err instanceof Error ? err.message : String(err);
    }
  };

  const importButton = el("button", {
    text: "Import…",
    onclick: () => {
      statusEl.textContent = "";
      clearProgress();
      fileInput.value = "";
      fileInput.click();
    },
  }) as HTMLButtonElement;

  fileInput.addEventListener("change", async () => {
    const file = fileInput.files?.[0];
    if (!file) return;

    statusEl.textContent = "";
    importButton.disabled = true;
    clearProgress();
    setProgress(0, file.size);

    try {
      const desiredName = importNameInput.value.trim();
      const imported = await store.import(
        file,
        desiredName.length > 0 ? desiredName : undefined,
        ({ loaded, total }) => setProgress(loaded, total),
      );
      statusEl.textContent = `Imported "${imported.name}" (${formatByteSize(imported.size)})`;
      if (!getActiveDiskName()) setActiveDiskName(imported.name);
      await refreshList();
    } catch (err) {
      statusEl.textContent = err instanceof Error ? err.message : String(err);
    } finally {
      importButton.disabled = false;
      clearProgress();
    }
  });

  const openWorkerStatus = el("span", { class: "muted", text: "" }) as HTMLSpanElement;
  const openWorkerButton = el("button", {
    text: "Open active disk in I/O worker",
    onclick: async () => {
      const activeName = getActiveDiskName();
      if (!activeName) {
        openWorkerStatus.textContent = "No active disk selected.";
        return;
      }

      openWorkerButton.disabled = true;
      openWorkerStatus.textContent = "Opening…";

      try {
        const tokenOrHandle = await store.openForWorker(activeName);
        if (!tokenOrHandle || typeof tokenOrHandle !== "object" || !("kind" in tokenOrHandle)) {
          throw new Error("Disk store returned an unsupported worker handle descriptor.");
        }

        const result = await ioWorker.openActiveDisk(tokenOrHandle as WorkerOpenToken);
        openWorkerStatus.textContent = result.syncAccessHandleAvailable
          ? `Opened (size: ${formatByteSize(result.size)}).`
          : `Opened without sync access handle (size: ${formatByteSize(result.size)}).`;
      } catch (err) {
        openWorkerStatus.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        openWorkerButton.disabled = false;
      }
    },
  }) as HTMLButtonElement;

  const table = el(
    "table",
    {},
    el(
      "thead",
      {},
      el(
        "tr",
        {},
        el("th", { text: "Active" }),
        el("th", { text: "Name" }),
        el("th", { text: "Size" }),
        el("th", { text: "Actions" }),
      ),
    ),
    tableBody,
  );

  void refreshList();

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Disk Images" }),
    warningEl,
    el("div", { class: "row" }, el("label", { text: "Name:" }), importNameInput, importButton, fileInput),
    el("div", { class: "row" }, importProgress, importProgressText),
    el("div", { class: "row" }, openWorkerButton, openWorkerStatus),
    el("div", { class: "row" }, statusEl),
    table,
  );
}

function renderRemoteDiskPanel(): HTMLElement {
  const warning = el(
    "div",
    { class: "mono" },
    "Remote disk images are experimental. Only use images you own/have rights to. ",
    "The server must support either HTTP Range requests (single-file images) or the chunked manifest format (see docs/disk-images.md). ",
    "For local testing, run `docker compose up` in `infra/local-object-store/` and upload an object to `disk-images/`. ",
    "For CDN/edge emulation, run `docker compose --profile proxy up` and use port 9002.",
  );

  const enabledInput = el("input", { type: "checkbox" }) as HTMLInputElement;
  const modeSelect = el(
    "select",
    {},
    el("option", { value: "range", text: "HTTP Range" }),
    el("option", { value: "chunked", text: "Chunked manifest.json" }),
  ) as HTMLSelectElement;
  const cacheBackendSelect = el(
    "select",
    {},
    el("option", { value: "auto", text: "cache: auto" }),
    el("option", { value: "opfs", text: "cache: OPFS" }),
    el("option", { value: "idb", text: "cache: IndexedDB" }),
  ) as HTMLSelectElement;
  const urlInput = el("input", { type: "url", placeholder: "http://localhost:9000/disk-images/large.bin" }) as HTMLInputElement;
  const blockSizeInput = el("input", { type: "number", value: String(1024), min: "4" }) as HTMLInputElement;
  const cacheLimitInput = el("input", { type: "number", value: String(512), min: "0" }) as HTMLInputElement;
  const prefetchInput = el("input", { type: "number", value: String(2), min: "0" }) as HTMLInputElement;
  const maxConcurrentFetchesInput = el("input", { type: "number", value: String(4), min: "1" }) as HTMLInputElement;
  const stats = el("pre", { text: "" });
  const output = el("pre", { text: "" });

  const probeButton = el("button", { text: "Probe Range support" }) as HTMLButtonElement;
  const readButton = el("button", { text: "Read sample bytes" }) as HTMLButtonElement;
  const flushButton = el("button", { text: "Flush cache" }) as HTMLButtonElement;
  const clearButton = el("button", { text: "Clear cache" }) as HTMLButtonElement;
  const closeButton = el("button", { text: "Close" }) as HTMLButtonElement;
  const progress = el("progress", { value: "0", max: "1", style: "width: 320px" }) as HTMLProgressElement;

  const client = new RuntimeDiskClient();
  let handle: number | null = null;
  let statsPollPending = false;

  function updateButtons(): void {
    const enabled = enabledInput.checked;
    probeButton.disabled = !enabled;
    readButton.disabled = !enabled;
    flushButton.disabled = !enabled || handle === null;
    clearButton.disabled = !enabled || handle === null;
    closeButton.disabled = !enabled || handle === null;
  }

  function updateModeUi(): void {
    const chunked = modeSelect.value === "chunked";
    blockSizeInput.disabled = chunked;
    maxConcurrentFetchesInput.disabled = !chunked;
    urlInput.placeholder = chunked
      ? "http://localhost:9000/disk-images/manifest.json"
      : "http://localhost:9000/disk-images/large.bin";
    probeButton.textContent = chunked ? "Fetch manifest" : "Probe Range support";
  }

  enabledInput.addEventListener("change", () => {
    if (!enabledInput.checked) {
      void closeHandle();
    }
    updateButtons();
  });
  modeSelect.addEventListener("change", () => {
    void closeHandle();
    updateModeUi();
    updateButtons();
  });
  updateModeUi();
  updateButtons();

  async function closeHandle(): Promise<void> {
    if (handle === null) return;
    const cur = handle;
    handle = null;
    try {
      await client.closeDisk(cur);
    } catch (err) {
      output.textContent = err instanceof Error ? err.message : String(err);
    }
  }

  async function ensureOpen(): Promise<number> {
    if (handle !== null) return handle;
    const url = urlInput.value.trim();
    if (!url) throw new Error("Enter a URL first.");

    const cacheLimitMiB = Number(cacheLimitInput.value);
    const cacheLimitBytes = cacheLimitMiB <= 0 ? null : cacheLimitMiB * 1024 * 1024;

    const prefetchSequential = Math.max(0, Number(prefetchInput.value) | 0);
    const opened =
      modeSelect.value === "chunked"
        ? await client.openChunked(url, {
            cacheLimitBytes,
            prefetchSequentialChunks: prefetchSequential,
            maxConcurrentFetches: Math.max(1, Number(maxConcurrentFetchesInput.value) | 0),
            cacheBackend: cacheBackendSelect.value === "auto" ? undefined : (cacheBackendSelect.value as "opfs" | "idb"),
          })
        : await client.openRemote(url, {
            blockSize: Number(blockSizeInput.value) * 1024,
            cacheLimitBytes,
            prefetchSequentialBlocks: prefetchSequential,
            cacheBackend: cacheBackendSelect.value === "auto" ? undefined : (cacheBackendSelect.value as "opfs" | "idb"),
          });
    handle = opened.handle;
    updateButtons();
    return opened.handle;
  }

  async function refreshStats(): Promise<void> {
    if (!enabledInput.checked || handle === null) {
      stats.textContent = "";
      return;
    }
    if (statsPollPending) return;
    statsPollPending = true;
    const cur = handle;
    try {
      const res = await client.stats(cur);
      if (handle !== cur) return;

      const remote = res.remote;
      if (!remote) {
        stats.textContent = `diskSize=${formatBytes(res.capacityBytes)} reads=${res.io.reads} writes=${res.io.writes}`;
        return;
      }

      const hitRateDenom = remote.cacheHits + remote.cacheMisses;
      const hitRate = hitRateDenom > 0 ? remote.cacheHits / hitRateDenom : 0;
      const cacheCoverage = remote.totalSize > 0 ? remote.cachedBytes / remote.totalSize : 0;
      const cacheLimitText = remote.cacheLimitBytes === null ? "off" : formatBytes(remote.cacheLimitBytes);
      const downloadAmplification = res.io.bytesRead > 0 ? remote.bytesDownloaded / res.io.bytesRead : 0;

      stats.textContent =
        `imageSize=${formatBytes(remote.totalSize)}\n` +
        `cache=${formatBytes(remote.cachedBytes)} (${(cacheCoverage * 100).toFixed(2)}%) limit=${cacheLimitText}\n` +
        `blockSize=${formatBytes(remote.blockSize)}\n` +
        `ioReads=${res.io.reads} inflightReads=${res.io.inflightReads} lastReadMs=${res.io.lastReadMs === null ? "—" : res.io.lastReadMs.toFixed(1)}\n` +
        `ioBytesRead=${formatBytes(res.io.bytesRead)} downloadAmp=${downloadAmplification.toFixed(2)}x\n` +
        `requests=${remote.requests} bytesDownloaded=${formatBytes(remote.bytesDownloaded)}\n` +
        `blockRequests=${remote.blockRequests} hits=${remote.cacheHits} misses=${remote.cacheMisses} inflightJoins=${remote.inflightJoins} hitRate=${(hitRate * 100).toFixed(1)}%\n` +
        `inflightFetches=${remote.inflightFetches} lastFetchMs=${remote.lastFetchMs === null ? "—" : remote.lastFetchMs.toFixed(1)}`;
    } catch (err) {
      stats.textContent = err instanceof Error ? err.message : String(err);
    } finally {
      statsPollPending = false;
    }
  }

  window.setInterval(() => void refreshStats(), 250);

  probeButton.onclick = async () => {
    output.textContent = "";
    progress.value = 0;

    try {
      await closeHandle();

      output.textContent = "Probing… (this will make HTTP requests)\n";
      const openedHandle = await ensureOpen();
      const res = await client.stats(openedHandle);
      output.textContent = JSON.stringify(res.remote, null, 2);
      updateButtons();
    } catch (err) {
      output.textContent = err instanceof Error ? err.message : String(err);
    }
  };

  readButton.onclick = async () => {
    output.textContent = "";
    progress.value = 0;

    try {
      const openedHandle = await ensureOpen();

      const bytes = await client.read(openedHandle, 2, 512);
      const res = await client.stats(openedHandle);
      output.textContent = JSON.stringify(
        { read: { lba: 2, byteLength: 512, first16: Array.from(bytes.slice(0, 16)) }, stats: res.remote },
        null,
        2,
      );
      progress.value = 1;
    } catch (err) {
      output.textContent = err instanceof Error ? err.message : String(err);
    }
  };

  flushButton.onclick = async () => {
    output.textContent = "";
    progress.value = 0;
    try {
      if (handle === null) {
        output.textContent = "Nothing to flush (probe/open first).";
        return;
      }
      await client.flush(handle);
      progress.value = 1;
      void refreshStats();
    } catch (err) {
      output.textContent = err instanceof Error ? err.message : String(err);
    }
  };

  clearButton.onclick = async () => {
    output.textContent = "";
    progress.value = 0;
    try {
      if (handle === null) {
        output.textContent = "Nothing to clear (probe/open first).";
        return;
      }
      await client.clearCache(handle);
      progress.value = 1;
      output.textContent = "Cache cleared.";
      void refreshStats();
      updateButtons();
    } catch (err) {
      output.textContent = err instanceof Error ? err.message : String(err);
    }
  };

  closeButton.onclick = async () => {
    output.textContent = "";
    progress.value = 0;
    await closeHandle();
    updateButtons();
  };

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Remote disk image (streaming)" }),
    warning,
    el(
      "div",
      { class: "row" },
      el("label", { text: "Enable:" }),
      enabledInput,
      el("label", { text: "Mode:" }),
      modeSelect,
      cacheBackendSelect,
      el("label", { text: "URL:" }),
      urlInput,
    ),
    el(
      "div",
      { class: "row" },
      el("label", { text: "Block KiB (range):" }),
      blockSizeInput,
      el("label", { text: "Cache MiB (0=off):" }),
      cacheLimitInput,
      el("label", { text: "Prefetch:" }),
      prefetchInput,
      el("label", { text: "Max inflight (chunked):" }),
      maxConcurrentFetchesInput,
      probeButton,
      readButton,
      flushButton,
      clearButton,
      closeButton,
      progress,
    ),
    stats,
    output,
  );
}

function renderAudioPanel(): HTMLElement {
  const status = el("pre", { text: "" });
  let toneTimer: number | null = null;
  let tonePhase = 0;
  let toneGeneration = 0;
  let wasmBridge: unknown | null = null;
  let wasmTone: { free(): void } | null = null;
  let stopPerfSampling: (() => void) | null = null;

  function stopTone() {
    toneGeneration += 1;
    if (toneTimer !== null) {
      window.clearInterval(toneTimer);
      toneTimer = null;
    }
    if (stopPerfSampling) {
      stopPerfSampling();
      stopPerfSampling = null;
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
    const writeToneJs = (frames: number) => {
      const buf = new Float32Array(frames * channelCount);
      for (let i = 0; i < frames; i++) {
        const s = Math.sin(tonePhase * 2 * Math.PI) * gain;
        for (let c = 0; c < channelCount; c++) buf[i * channelCount + c] = s;
        tonePhase += freqHz / sr;
        if (tonePhase >= 1) tonePhase -= 1;
      }
      output.writeInterleaved(buf, sr);
    };

    let writeTone = writeToneJs;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroAudioToneBackend = "js";

    const gen = toneGeneration;
    void wasmInitPromise
      .then(({ api }) => {
        if (toneGeneration !== gen) return;
        if (
          typeof api.attach_worklet_bridge !== "function" ||
          typeof api.SineTone !== "function" ||
          !(output.ringBuffer.buffer instanceof SharedArrayBuffer)
        ) {
          return;
        }

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
      })
      .catch(() => {
        // Keep JS fallback.
      });

    const buffering = createAdaptiveRingBufferTarget(output.ringBuffer.capacityFrames, sr);

    // Prefill ~100ms (or up to capacity) to avoid startup underruns, then allow
    // the adaptive target to converge downward.
    writeTone(Math.min(output.ringBuffer.capacityFrames, Math.floor(sr / 10)));

    toneTimer = window.setInterval(() => {
      const underruns = output.getUnderrunCount();
      const level = output.getBufferLevelFrames();
      const target = buffering.update(level, underruns);
      const need = Math.max(0, target - level);
      if (need > 0) writeTone(need);

      const metrics = output.getMetrics();
      status.textContent =
        `AudioContext: ${metrics.state}\n` +
        `sampleRate: ${metrics.sampleRate}\n` +
        `capacityFrames: ${metrics.capacityFrames}\n` +
        `targetFrames: ${target}\n` +
        `bufferLevelFrames: ${metrics.bufferLevelFrames}\n` +
        `targetMs: ${((target / sr) * 1000).toFixed(1)}\n` +
        `bufferLevelMs: ${((metrics.bufferLevelFrames / sr) * 1000).toFixed(1)}\n` +
        `underruns: ${metrics.underrunCount}\n` +
        `overruns: ${metrics.overrunCount}`;
    }, 20);
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
        if (window.aero?.perf) {
          stopPerfSampling?.();
          stopPerfSampling = startAudioPerfSampling(output, perf);
        }
      } catch (err) {
        stopTone();
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }
      status.textContent = "Audio initialized and test tone started.";
    },
  });

  const workerButton = el("button", {
    id: "init-audio-output-worker",
    text: "Init audio output (worker tone)",
    onclick: async () => {
      status.textContent = "";
      stopTone();

      try {
        workerCoordinator.start(configManager.getState().effective);
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }

      const output = await createAudioOutput({
        sampleRate: 48_000,
        latencyHint: "interactive",
        ringBufferFrames: Math.floor(48_000 / 5),
      });
      // Expose for Playwright smoke tests.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioOutputWorker = output;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioToneBackendWorker = "cpu-worker-wasm";
      if (!output.enabled) {
        status.textContent = output.message;
        return;
      }

      try {
        // Prefill the entire ring with silence so the CPU worker has time to attach
        // and begin writing without incurring startup underruns.
        output.writeInterleaved(
          new Float32Array(output.ringBuffer.capacityFrames * output.ringBuffer.channelCount),
          output.context.sampleRate,
        );

        workerCoordinator.setAudioOutputRingBuffer(
          output.ringBuffer.buffer,
          output.context.sampleRate,
          output.ringBuffer.channelCount,
          output.ringBuffer.capacityFrames,
        );

        await output.resume();
        if (window.aero?.perf) {
          stopPerfSampling?.();
          stopPerfSampling = startAudioPerfSampling(output, perf);
        }
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        return;
      }

      toneTimer = window.setInterval(() => {
        const metrics = output.getMetrics();
        status.textContent =
          `AudioContext: ${metrics.state}\n` +
          `sampleRate: ${metrics.sampleRate}\n` +
          `capacityFrames: ${metrics.capacityFrames}\n` +
          `bufferLevelFrames: ${metrics.bufferLevelFrames}\n` +
          `underruns: ${metrics.underrunCount}\n` +
          `overruns: ${metrics.overrunCount}\n` +
          `producer.bufferLevelFrames: ${workerCoordinator.getAudioProducerBufferLevelFrames()}\n` +
          `producer.underruns: ${workerCoordinator.getAudioProducerUnderrunCount()}`;
      }, 50);

      status.textContent = "Audio initialized (worker tone backend).";
    },
  });

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Audio" }),
    el("div", { class: "row" }, button, workerButton),
    status,
  );
}

function renderMicrophonePanel(): HTMLElement {
  const status = el("pre", { text: "" });
  const stateLine = el("div", { class: "mono", text: "state=inactive" });
  const statsLine = el("div", { class: "mono", text: "" });

  const deviceSelect = el("select") as HTMLSelectElement;
  const echoCancellation = el("input", { type: "checkbox", checked: "" }) as HTMLInputElement;
  const noiseSuppression = el("input", { type: "checkbox", checked: "" }) as HTMLInputElement;
  const autoGainControl = el("input", { type: "checkbox", checked: "" }) as HTMLInputElement;
  const bufferMsInput = el("input", { type: "number", value: "80", min: "10", max: "500", step: "10" }) as HTMLInputElement;
  const mutedInput = el("input", { type: "checkbox" }) as HTMLInputElement;

  let mic: MicCapture | null = null;
  let lastWorkletStats: { buffered?: number; dropped?: number } = {};

  async function refreshDevices(): Promise<void> {
    deviceSelect.replaceChildren(el("option", { value: "", text: "default" }));
    if (!navigator.mediaDevices?.enumerateDevices) return;
    const devices = await navigator.mediaDevices.enumerateDevices();
    for (const dev of devices) {
      if (dev.kind !== "audioinput") continue;
      const label = dev.label || `mic (${dev.deviceId.slice(0, 8)}…)`;
      deviceSelect.append(el("option", { value: dev.deviceId, text: label }));
    }
  }

  function attachToWorkers(): void {
    if (micAttachment) {
      workerCoordinator.setMicrophoneRingBuffer(micAttachment.ringBuffer, micAttachment.sampleRate);
    } else {
      workerCoordinator.setMicrophoneRingBuffer(null, 0);
    }
  }

  function update(): void {
    const state = mic?.state ?? "inactive";
    stateLine.textContent = `state=${state}`;

    const buffered = lastWorkletStats.buffered ?? 0;
    const dropped = lastWorkletStats.dropped ?? 0;
    statsLine.textContent =
      `bufferedSamples=${buffered} droppedSamples=${dropped} ` +
      `device=${deviceSelect.value ? deviceSelect.value.slice(0, 8) + "…" : "default"}`;
  }

  const startButton = el("button", {
    text: "Start microphone",
    onclick: async () => {
      status.textContent = "";
      lastWorkletStats = {};

      try {
        if (!navigator.mediaDevices?.getUserMedia) {
          throw new Error("getUserMedia is unavailable in this browser.");
        }
        if (typeof SharedArrayBuffer === "undefined") {
          throw new Error("SharedArrayBuffer is unavailable; microphone capture requires crossOriginIsolated.");
        }

        if (mic) {
          await mic.stop();
          mic = null;
        }

        mic = new MicCapture({
          sampleRate: 48_000,
          bufferMs: Math.max(10, Number(bufferMsInput.value || 0) | 0),
          preferWorklet: true,
          deviceId: deviceSelect.value || undefined,
          echoCancellation: echoCancellation.checked,
          noiseSuppression: noiseSuppression.checked,
          autoGainControl: autoGainControl.checked,
        });

        mic.addEventListener("statechange", update);
        mic.addEventListener("devicechange", () => {
          void refreshDevices();
        });
        mic.addEventListener("message", (event) => {
          const data = (event as MessageEvent).data as unknown;
          if (!data || typeof data !== "object") return;
          const msg = data as { type?: unknown; buffered?: unknown; dropped?: unknown };
          if (msg.type === "stats") {
            lastWorkletStats = {
              buffered: typeof msg.buffered === "number" ? msg.buffered : undefined,
              dropped: typeof msg.dropped === "number" ? msg.dropped : undefined,
            };
            update();
          }
        });

        await mic.start();
        mic.setMuted(mutedInput.checked);

        micAttachment = { ringBuffer: mic.ringBuffer.sab, sampleRate: mic.options.sampleRate };
        attachToWorkers();
        update();
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
        micAttachment = null;
        attachToWorkers();
        update();
      }
    },
  }) as HTMLButtonElement;

  const stopButton = el("button", {
    text: "Stop microphone",
    onclick: async () => {
      status.textContent = "";
      try {
        await mic?.stop();
        mic = null;
      } catch (err) {
        status.textContent = err instanceof Error ? err.message : String(err);
      } finally {
        micAttachment = null;
        attachToWorkers();
        update();
      }
    },
  }) as HTMLButtonElement;

  mutedInput.addEventListener("change", () => {
    mic?.setMuted(mutedInput.checked);
    update();
  });

  void refreshDevices().then(update);
  navigator.mediaDevices?.addEventListener?.("devicechange", () => void refreshDevices());

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Microphone (capture)" }),
    el("div", { class: "row" }, startButton, stopButton, el("label", { text: "device:" }), deviceSelect),
    el(
      "div",
      { class: "row" },
      el("label", { text: "echoCancellation:" }),
      echoCancellation,
      el("label", { text: "noiseSuppression:" }),
      noiseSuppression,
      el("label", { text: "autoGainControl:" }),
      autoGainControl,
      el("label", { text: "bufferMs:" }),
      bufferMsInput,
      el("label", { text: "mute:" }),
      mutedInput,
    ),
    stateLine,
    statsLine,
    status,
  );
}

function renderWebHidPassthroughPanel(): HTMLElement {
  const host = el("div");
  mountWebHidPassthroughPanel(host, new WebHidPassthroughManager());
  return el("div", { class: "panel" }, host);
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
  const status = el("div", { class: "mono", text: "" });
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

  const inputTarget: InputBatchTarget = {
    postMessage: (msg, transfer) => {
      const words = new Int32Array(msg.buffer);
      const count = words[0] >>> 0;
      const base = 2;
      for (let i = 0; i < count; i += 1) {
        const off = base + i * 4;
        const type = words[off] >>> 0;
        if (type === InputEventType.KeyScancode) {
          const packed = words[off + 2] >>> 0;
          const len = words[off + 3] >>> 0;
          const bytes = [];
          for (let j = 0; j < len; j += 1) bytes.push((packed >>> (j * 8)) & 0xff);
          append(`kbd: ${bytes.map((b) => b.toString(16).padStart(2, "0")).join(" ")}`);
        } else if (type === InputEventType.GamepadReport) {
          const lo = words[off + 2] >>> 0;
          const hi = words[off + 3] >>> 0;
          const bytes = [];
          for (let j = 0; j < 4; j += 1) bytes.push((lo >>> (j * 8)) & 0xff);
          for (let j = 0; j < 4; j += 1) bytes.push((hi >>> (j * 8)) & 0xff);
          append(`pad: ${bytes.map((b) => b.toString(16).padStart(2, "0")).join(" ")}`);
        } else if (type === InputEventType.MouseButtons) {
          append(`mouse: buttons=0x${(words[off + 2] >>> 0).toString(16)}`);
        } else if (type === InputEventType.MouseWheel) {
          append(`mouse: wheel=${words[off + 2] | 0}`);
        }
      }

      const ioWorker = workerCoordinator.getIoWorker();
      if (ioWorker) {
        ioWorker.postMessage(msg, transfer);
      }
    },
  };

  const capture = new InputCapture(canvas, inputTarget);
  capture.start();

  const hint = el("div", {
    class: "mono",
    text: "Click the canvas to focus + request pointer lock. Keyboard/mouse/gamepad events are batched and forwarded to the I/O worker.",
  });

  const clear = el("button", {
    text: "Clear log",
    onclick: () => {
      log.textContent = "";
    },
  });

  const updateStatus = (): void => {
    status.textContent =
      `pointerLock=${capture.pointerLocked ? "yes" : "no"}  ` +
      `ioWorker=${workerCoordinator.getIoWorker() ? "ready" : "stopped"}  ` +
      `ioBatches=${workerCoordinator.getIoInputBatchCounter()}  ` +
      `ioEvents=${workerCoordinator.getIoInputEventCounter()}`;
  };
  updateStatus();
  globalThis.setInterval(updateStatus, 250);

  return el(
    "div",
    { class: "panel" },
    el("h2", { text: "Input capture (PS/2 + USB HID gamepad reports)" }),
    hint,
    status,
    el("div", { class: "row" }, clear),
    canvas,
    log,
  );
}

function renderWorkersPanel(report: PlatformFeatureReport): HTMLElement {
  const support = workerCoordinator.checkSupport();

  const statusList = el("ul");
  const heartbeatLine = el("div", { class: "mono", text: "" });
  const frameLine = el("div", { class: "mono", text: "" });
  const error = el("pre", { text: "" });
  const guestRamValue = el("span", { class: "mono", text: "" });
  const jitDemoLine = el("div", { class: "mono", text: "jit: (idle)" });
  const jitDemoError = el("pre", { text: "" });

  const forceJitCspBlock = el("input", { type: "checkbox" }) as HTMLInputElement;
  const forceJitCspLabel = el("label", { class: "mono", text: "force jit_dynamic_wasm=false" });

  const JIT_DEMO_WASM_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
  let jitDemoInFlight = false;
  let jitClient: JitWorkerClient | null = null;
  let jitClientWorker: Worker | null = null;

  async function runJitCompileDemo(): Promise<void> {
    const jitWorker = workerCoordinator.getWorker("jit");
    if (!jitWorker) {
      jitDemoError.textContent = "JIT worker is not running.";
      return;
    }

    if (!jitClient || jitClientWorker !== jitWorker) {
      jitClient = new JitWorkerClient(jitWorker);
      jitClientWorker = jitWorker;
    }

    jitDemoError.textContent = "";
    jitDemoLine.textContent = "jit: compiling…";
    jitDemoInFlight = true;
    update();

    const wasmBytes = JIT_DEMO_WASM_BYTES.slice().buffer;

    let response: JitWorkerResponse;
    try {
      response = await jitClient.compile(wasmBytes, { timeoutMs: 5000 });
    } catch (err) {
      jitDemoError.textContent = err instanceof Error ? err.message : String(err);
      jitDemoLine.textContent = "jit: demo failed";
      return;
    } finally {
      jitDemoInFlight = false;
      update();
    }

    // Expose for Playwright / devtools.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroJitDemo = response;

    if (response.type === "jit:error") {
      jitDemoLine.textContent = `jit: error (${response.code ?? "unknown"}) in ${response.durationMs ?? 0}ms`;
      jitDemoError.textContent = response.message;
      return;
    }

    // Verify that the module is usable in this realm (compilation happens in the JIT worker).
    try {
      if (!(response.module instanceof WebAssembly.Module)) {
        throw new Error("Response module is not a WebAssembly.Module.");
      }
      // Instantiation is cheap for the empty module, but keep it async.
      await WebAssembly.instantiate(response.module, {});
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      jitDemoLine.textContent = "jit: compiled, but validation failed";
      jitDemoError.textContent = message;
      return;
    }

    const cached = response.cached ? " (cached)" : "";
    jitDemoLine.textContent = `jit: compiled demo module in ${response.durationMs.toFixed(2)}ms${cached}`;
  }

  const jitDemoButton = el("button", {
    text: "Test JIT compile",
    onclick: () => {
      void runJitCompileDemo();
    },
  }) as HTMLButtonElement;

  const startButton = el("button", {
    text: "Start workers",
    onclick: () => {
      error.textContent = "";
      const config = configManager.getState().effective;
      try {
        const platformFeatures = forceJitCspBlock.checked ? { ...report, jit_dynamic_wasm: false } : report;
        workerCoordinator.start(config, { platformFeatures });
        const gpuWorker = workerCoordinator.getWorker("gpu");
        const frameStateSab = workerCoordinator.getFrameStateSab();
        const sharedFramebuffer = workerCoordinator.getVgaFramebuffer();
        if (gpuWorker && frameStateSab && sharedFramebuffer) {
          // Reset any previously transferred canvas before re-attaching it to a
          // new worker.
          if (canvasTransferred) resetVgaCanvas();

          let offscreen: OffscreenCanvas | undefined;
          useWorkerPresentation = false;
          if (
            report.offscreenCanvas &&
            "transferControlToOffscreen" in vgaCanvas &&
            typeof (vgaCanvas as unknown as { transferControlToOffscreen?: unknown }).transferControlToOffscreen ===
              "function"
          ) {
            try {
              offscreen = (vgaCanvas as unknown as HTMLCanvasElement & { transferControlToOffscreen: () => OffscreenCanvas })
                .transferControlToOffscreen();
              canvasTransferred = true;
              useWorkerPresentation = true;
            } catch {
              // Ignore and fall back to main-thread presentation.
              offscreen = undefined;
              canvasTransferred = false;
              useWorkerPresentation = false;
            }
          }

          frameScheduler?.stop();
          frameScheduler = startFrameScheduler({
            gpuWorker,
            sharedFrameState: frameStateSab,
            sharedFramebuffer,
            sharedFramebufferOffsetBytes: 0,
            canvas: offscreen,
            initOptions: offscreen
              ? {
                  outputWidth: 640,
                  outputHeight: 480,
                  dpr: window.devicePixelRatio || 1,
                }
              : undefined,
            showDebugOverlay: true,
          });
        }
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
      }
      update();
    },
  }) as HTMLButtonElement;

  const stopButton = el("button", {
    text: "Stop workers",
    onclick: () => {
      frameScheduler?.stop();
      frameScheduler = null;
      workerCoordinator.stop();
      useWorkerPresentation = false;
      teardownVgaPresenter();
      if (canvasTransferred) resetVgaCanvas();
      update();
    },
  }) as HTMLButtonElement;

  const hint = el("div", {
    class: "mono",
    text: support.ok
      ? "Runs 4 module workers (cpu/gpu/io/jit). CPU increments a shared WebAssembly.Memory counter + emits ring-buffer heartbeats."
      : support.reason ?? "SharedArrayBuffer unavailable.",
  });

  const createVgaCanvas = (): HTMLCanvasElement => {
    const canvas = el("canvas") as HTMLCanvasElement;
    canvas.style.width = "640px";
    canvas.style.height = "480px";
    canvas.style.border = "1px solid #333";
    canvas.style.background = "#000";
    canvas.style.imageRendering = "pixelated";
    return canvas;
  };

  let vgaCanvas = createVgaCanvas();
  const vgaCanvasRow = el("div", { class: "row" }, vgaCanvas);
  let canvasTransferred = false;
  let useWorkerPresentation = false;

  function resetVgaCanvas(): void {
    // `transferControlToOffscreen()` is one-shot per HTMLCanvasElement. When the
    // worker presentation path is used, recreate the canvas so stop/start cycles
    // continue to work.
    vgaCanvas = createVgaCanvas();
    vgaCanvasRow.replaceChildren(vgaCanvas);
    canvasTransferred = false;
  }

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

    if (useWorkerPresentation) {
      // Worker owns the canvas; main-thread presenter must be disabled.
      if (vgaPresenter) {
        vgaPresenter.destroy();
        vgaPresenter = null;
      }
      return;
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
    const config = configManager.getState().effective;

    startButton.disabled = !support.ok || !report.wasmThreads || !config.enableWorkers || anyActive;
    stopButton.disabled = !anyActive;
    jitDemoButton.disabled = statuses.jit.state !== "ready" || jitDemoInFlight;
    forceJitCspBlock.disabled = anyActive;

    statusList.replaceChildren(
      ...Object.entries(statuses).map(([role, status]) => {
        const roleName = role as WorkerRole;
        const wasm = workerCoordinator.getWorkerWasmStatus(roleName);
        const wasmSuffix =
          roleName === "cpu" && status.state !== "stopped"
            ? wasm
              ? ` wasm(${wasm.variant}) add(20,22)=${wasm.value}`
              : " wasm(pending)"
            : "";
        return el("li", {
          text: `${roleName}: ${status.state}${status.error ? ` (${status.error})` : ""}${wasmSuffix}`,
        });
      }),
    );

    heartbeatLine.textContent =
      `config[v${workerCoordinator.getConfigVersion()}]  ` +
      `status[HeartbeatCounter]=${workerCoordinator.getHeartbeatCounter()}  ` +
      `ring[Heartbeat]=${workerCoordinator.getLastHeartbeatFromRing()}  ` +
      `guestI32[0]=${workerCoordinator.getGuestCounter0()}`;

    const frameStateSab = workerCoordinator.getFrameStateSab();
    if (!frameStateSab) {
      frameLine.textContent = "frame: (uninitialized)";
    } else {
      const frameState = new Int32Array(frameStateSab);
      frameLine.textContent = `frame: status=${Atomics.load(frameState, FRAME_STATUS_INDEX)} seq=${Atomics.load(frameState, FRAME_SEQ_INDEX)}`;
    }
    guestRamValue.textContent =
      config.guestMemoryMiB % 1024 === 0 ? `${config.guestMemoryMiB / 1024} GiB` : `${config.guestMemoryMiB} MiB`;

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
    el(
      "div",
      { class: "row" },
      el("label", { text: "Guest RAM:" }),
      guestRamValue,
      startButton,
      stopButton,
      jitDemoButton,
      forceJitCspBlock,
      forceJitCspLabel,
    ),
    vgaCanvasRow,
    vgaInfoLine,
    heartbeatLine,
    frameLine,
    jitDemoLine,
    jitDemoError,
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
