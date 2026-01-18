/**
 * Lightweight on-screen debug overlay for GPU telemetry.
 *
 * This overlay is designed to be "always safe" to include: it does no work when
 * hidden, and renders from a structured telemetry snapshot.
 */

import { aerogpuFormatToString } from "../../emulator/protocol/aerogpu/aerogpu_pci.ts";

function fmtFixed(n: number | null, digits: number): string {
  if (n == null || !Number.isFinite(n)) return "n/a";
  return n.toFixed(digits);
}

function fmtMs(ms: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return "n/a";
  if (ms >= 1000) return `${fmtFixed(ms / 1000, 2)}s`;
  if (ms >= 10) return `${fmtFixed(ms, 1)}ms`;
  return `${fmtFixed(ms, 2)}ms`;
}

function fmtBytes(bytes: number | null): string {
  if (bytes == null || !Number.isFinite(bytes)) return "n/a";
  const abs = Math.abs(bytes);
  if (abs >= 1024 * 1024 * 1024) return `${fmtFixed(bytes / (1024 * 1024 * 1024), 2)} GiB`;
  if (abs >= 1024 * 1024) return `${fmtFixed(bytes / (1024 * 1024), 2)} MiB`;
  if (abs >= 1024) return `${fmtFixed(bytes / 1024, 2)} KiB`;
  return `${fmtFixed(bytes, 0)} B`;
}

function fmtPct(value: number | null): string {
  if (value == null || !Number.isFinite(value)) return "n/a";
  return `${fmtFixed(value * 100, 1)}%`;
}

function fmtScanoutSource(source: number | null): string {
  if (source == null || !Number.isFinite(source)) return "n/a";
  switch (source | 0) {
    case 0:
      return "LegacyText";
    case 1:
      return "LegacyVbeLfb";
    case 2:
      return "Wddm";
    default:
      return String(source);
  }
}

type GpuTelemetrySnapshot = any;

export type DebugOverlayOptions = {
  toggleKey?: string;
  updateIntervalMs?: number;
  parent?: HTMLElement;
};

export class DebugOverlay {
  private _getSnapshot: () => GpuTelemetrySnapshot | null;
  private _toggleKey: string;
  private _updateIntervalMs: number;
  private _parent: HTMLElement | null;

  private _visible = false;
  private _root: HTMLDivElement | null = null;
  private _interval: number | null = null;

  private _onKeyDown: (ev: KeyboardEvent) => void;

  constructor(getSnapshot: () => GpuTelemetrySnapshot | null, opts: DebugOverlayOptions = {}) {
    this._getSnapshot = getSnapshot;
    this._toggleKey = "F2";
    this._updateIntervalMs = 250;
    this._parent = null;

    this._toggleKey = opts.toggleKey ?? this._toggleKey;
    this._updateIntervalMs = opts.updateIntervalMs ?? this._updateIntervalMs;
    this._parent = opts.parent ?? (typeof document !== "undefined" ? document.body : null);

    const isTextInput = (target: EventTarget | null): boolean => {
      const HTMLElementCtor = (globalThis as unknown as { HTMLElement?: unknown }).HTMLElement;
      if (typeof HTMLElementCtor !== "function") return false;
      if (!(target instanceof (HTMLElementCtor as typeof HTMLElement))) return false;
      const el = target as HTMLElement;
      const tag = el.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return true;
      return el.isContentEditable;
    };

    this._onKeyDown = (ev) => {
      if (ev.repeat) return;
      // If a VM/input-capture layer (or another hotkey handler) already consumed
      // the event, do not toggle the overlay.
      if (ev.defaultPrevented) return;
      if (isTextInput(ev.target)) return;
      // Avoid toggling on Ctrl/Alt/Meta chords to reduce conflicts with host/OS
      // shortcuts and preserve the ability to use capture modifiers.
      if (ev.ctrlKey || ev.altKey || ev.metaKey || ev.shiftKey) return;
      if (ev.code !== this._toggleKey) return;
      ev.preventDefault();
      ev.stopPropagation();
      this.toggle();
    };
  }

  attach(): void {
    if (this._root) return;
    if (!this._parent || typeof document === "undefined") return;

    const el = document.createElement("div");
    el.style.position = "fixed";
    el.style.left = "0";
    el.style.top = "0";
    el.style.zIndex = "99999";
    el.style.pointerEvents = "none";
    el.style.padding = "8px 10px";
    el.style.background = "rgba(0,0,0,0.75)";
    el.style.color = "#d6f5d6";
    el.style.font = "12px/1.35 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
    el.style.whiteSpace = "pre";
    el.style.borderBottomRightRadius = "8px";
    el.style.display = "none";

    this._root = el;
    this._parent.appendChild(el);

    // Attach in bubbling phase so capture-phase handlers (VM input capture) can
    // swallow keystrokes first via stopPropagation.
    window.addEventListener("keydown", this._onKeyDown);
  }

  detach(): void {
    if (!this._root) return;
    this.hide();
    window.removeEventListener("keydown", this._onKeyDown);
    this._root.remove();
    this._root = null;
  }

  show(): void {
    this.attach();
    if (!this._root) return;
    if (this._visible) return;

    this._visible = true;
    this._root.style.display = "block";

    this._interval = window.setInterval(() => this._render(), this._updateIntervalMs);
    this._render();
  }

  hide(): void {
    if (!this._root) return;
    if (!this._visible) return;

    this._visible = false;
    this._root.style.display = "none";

    if (this._interval != null) {
      window.clearInterval(this._interval);
      this._interval = null;
    }
  }

  toggle(): void {
    if (this._visible) this.hide();
    else this.show();
  }

  private _render(): void {
    if (!this._root) return;

    const s = this._getSnapshot();
    if (!s) {
      this._root.textContent = "GPU telemetry: n/a";
      return;
    }

    const frame = s.frameTimeMs?.stats ?? null;
    const present = s.presentLatencyMs?.stats ?? null;
    const dxbc = s.shaderTranslationMs?.stats ?? null;
    const wgsl = s.shaderCompilationMs?.stats ?? null;

    const wallTimeMs = s.wallTimeTotalMs ?? null;
    const fps =
      wallTimeMs != null && wallTimeMs > 0 && frame?.count
        ? frame.count / (wallTimeMs / 1000)
        : frame?.mean
          ? 1000 / frame.mean
          : null;
    const up = s.textureUpload ?? null;
    const upAvgBytesPerFrame = up?.bytesPerFrame?.stats?.mean ?? null;

    const lines: string[] = [];
    lines.push(`GPU Telemetry (toggle: ${this._toggleKey})`);
    const framesReceived = typeof s.framesReceived === "number" ? s.framesReceived : null;
    const framesPresented = typeof s.framesPresented === "number" ? s.framesPresented : null;
    const framesDropped = typeof s.framesDropped === "number" ? s.framesDropped : null;
    if (framesReceived != null || framesPresented != null || framesDropped != null) {
      lines.push(
        `Frame pacing: received ${framesReceived ?? "n/a"}  presented ${framesPresented ?? "n/a"}  dropped ${framesDropped ?? "n/a"}`,
      );
    }

    const gpuStats = (s as Record<string, unknown>).gpuStats;
    const gpuStatsRecord = gpuStats && typeof gpuStats === "object" ? (gpuStats as Record<string, unknown>) : null;
    const backendKind = typeof gpuStatsRecord?.backendKind === "string" ? gpuStatsRecord.backendKind : null;
    if (backendKind) {
      lines.push(`Backend: ${backendKind}`);
    }
    const counters =
      gpuStatsRecord?.counters && typeof gpuStatsRecord.counters === "object"
        ? (gpuStatsRecord.counters as Record<string, unknown>)
        : null;
    if (counters) {
      const presentsAttempted = typeof counters.presents_attempted === "number" ? counters.presents_attempted : null;
      const presentsSucceeded = typeof counters.presents_succeeded === "number" ? counters.presents_succeeded : null;
      const recoveriesAttempted =
        typeof counters.recoveries_attempted === "number" ? counters.recoveries_attempted : null;
      const recoveriesSucceeded =
        typeof counters.recoveries_succeeded === "number" ? counters.recoveries_succeeded : null;
      const surfaceReconfigures =
        typeof counters.surface_reconfigures === "number" ? counters.surface_reconfigures : null;
      lines.push(
        `Presents: ${presentsSucceeded ?? "?"}/${presentsAttempted ?? "?"}  Recoveries: ${recoveriesSucceeded ?? "?"}/${recoveriesAttempted ?? "?"}  Surface reconfigures: ${surfaceReconfigures ?? "?"}`,
      );

      const recoveriesAttemptedWddm =
        typeof counters.recoveries_attempted_wddm === "number" ? counters.recoveries_attempted_wddm : null;
      const recoveriesSucceededWddm =
        typeof counters.recoveries_succeeded_wddm === "number" ? counters.recoveries_succeeded_wddm : null;
      if (recoveriesAttemptedWddm != null || recoveriesSucceededWddm != null) {
        lines.push(`Recoveries (WDDM): ${recoveriesSucceededWddm ?? "?"}/${recoveriesAttemptedWddm ?? "?"}`);
      }
    }

    const outputSource = typeof s.outputSource === "string" ? s.outputSource : null;
    const presentUpload =
      s.presentUpload && typeof s.presentUpload === "object" ? (s.presentUpload as Record<string, unknown>) : null;
    if (outputSource || presentUpload) {
      const uploadKind = typeof presentUpload?.kind === "string" ? presentUpload.kind : "n/a";
      const uploadDirtyCount = typeof presentUpload?.dirtyRectCount === "number" ? presentUpload.dirtyRectCount : null;
      const uploadDesc = uploadKind === "dirty_rects" ? `dirty_rects(n=${uploadDirtyCount ?? "?"})` : uploadKind;
      lines.push(`Presenter: source ${outputSource ?? "n/a"}  upload ${uploadDesc}`);
    }

    const scanout = s.scanout && typeof s.scanout === "object" ? (s.scanout as Record<string, unknown>) : null;
    if (scanout) {
      const base = typeof scanout.base_paddr === "string" ? scanout.base_paddr : "n/a";
      const gen = typeof scanout.generation === "number" ? scanout.generation : null;
      const src = typeof scanout.source === "number" ? scanout.source : null;
      const w = typeof scanout.width === "number" ? scanout.width : null;
      const h = typeof scanout.height === "number" ? scanout.height : null;
      const pitch = typeof scanout.pitchBytes === "number" ? scanout.pitchBytes : null;
      const fmtStr = typeof scanout.format_str === "string" ? scanout.format_str : null;
      const fmt = typeof scanout.format === "number" ? scanout.format : null;
      const fmtDesc = fmtStr ?? aerogpuFormatToString(fmt ?? Number.NaN);
      lines.push(
        `Scanout: ${fmtScanoutSource(src)} gen=${gen ?? "n/a"} base=${base} ${w ?? "?"}x${h ?? "?"} pitch=${pitch ?? "?"} fmt=${fmtDesc}`,
      );
    }

    // Best-effort: show the most recent structured GPU event, if the frame scheduler forwarded it.
    const gpuEventsRaw = (s as Record<string, unknown>).gpuEvents;
    const gpuEvents = Array.isArray(gpuEventsRaw) ? gpuEventsRaw : [];
    if (gpuEvents.length > 0) {
      const last = gpuEvents[gpuEvents.length - 1];
      const lastRecord = last && typeof last === "object" ? (last as Record<string, unknown>) : null;
      if (lastRecord) {
        const sev = typeof lastRecord.severity === "string" ? lastRecord.severity : "error";
        const cat = typeof lastRecord.category === "string" ? lastRecord.category : "Unknown";
        const backend = typeof lastRecord.backend_kind === "string" ? lastRecord.backend_kind : null;
        const msg = typeof lastRecord.message === "string" ? lastRecord.message : String(lastRecord.message ?? "");
        lines.push(`Last event: ${sev}/${cat}${backend ? ` (${backend})` : ""}: ${msg}`);
      }
    }

    lines.push(
      `Frames: ${frame?.count ?? 0}  Dropped: ${s.droppedFrames ?? 0}  FPS(avg): ${
        fps ? fmtFixed(fps, 1) : "n/a"
      }`,
    );
    lines.push(
      `Frame time: mean ${fmtMs(frame?.mean ?? null)}  p50 ${fmtMs(frame?.p50 ?? null)}  p95 ${fmtMs(
        frame?.p95 ?? null,
      )}`,
    );
    lines.push(
      `Present latency: p50 ${fmtMs(present?.p50 ?? null)}  p95 ${fmtMs(present?.p95 ?? null)}  n=${
        present?.count ?? 0
      }`,
    );
    lines.push(
      `DXBC→WGSL: mean ${fmtMs(dxbc?.mean ?? null)}  p95 ${fmtMs(dxbc?.p95 ?? null)}  n=${
        dxbc?.count ?? 0
      }`,
    );
    lines.push(
      `WGSL compile: mean ${fmtMs(wgsl?.mean ?? null)}  p95 ${fmtMs(wgsl?.p95 ?? null)}  n=${
        wgsl?.count ?? 0
      }`,
    );

    const cache = s.pipelineCache ?? null;
    lines.push(
      `Pipeline cache: hits ${cache?.hits ?? 0}  misses ${cache?.misses ?? 0}  hit ${fmtPct(
        cache?.hitRate ?? null,
      )}  entries ${cache?.entries ?? "n/a"}  size ${fmtBytes(cache?.sizeBytes ?? null)}`,
    );

    lines.push(
      `Texture upload: avg/frame ${fmtBytes(upAvgBytesPerFrame)}  avg BW ${fmtBytes(
        up?.bandwidthBytesPerSecAvg ?? null,
      )}/s  total ${fmtBytes(up?.bytesTotal ?? null)}`,
    );

    this._root.textContent = lines.join("\n");
  }
}

if (typeof globalThis !== "undefined") {
  const g = globalThis as unknown as Record<string, unknown>;
  const existing = g.AeroDebugOverlay;
  if (!existing || typeof existing !== "object") {
    g.AeroDebugOverlay = { DebugOverlay };
  } else if (!(existing as Record<string, unknown>).DebugOverlay) {
    (existing as Record<string, unknown>).DebugOverlay = DebugOverlay;
  }
}
