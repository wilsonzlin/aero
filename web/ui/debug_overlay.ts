/**
 * Lightweight on-screen debug overlay for GPU telemetry.
 *
 * This overlay is designed to be "always safe" to include: it does no work when
 * hidden, and renders from a structured telemetry snapshot.
 */

/**
 * @param {number} n
 * @param {number} digits
 */
function fmtFixed(n, digits) {
  if (n == null || !Number.isFinite(n)) return "n/a";
  return n.toFixed(digits);
}

/**
 * @param {number|null} ms
 */
function fmtMs(ms) {
  if (ms == null || !Number.isFinite(ms)) return "n/a";
  if (ms >= 1000) return `${fmtFixed(ms / 1000, 2)}s`;
  if (ms >= 10) return `${fmtFixed(ms, 1)}ms`;
  return `${fmtFixed(ms, 2)}ms`;
}

/**
 * @param {number|null} bytes
 */
function fmtBytes(bytes) {
  if (bytes == null || !Number.isFinite(bytes)) return "n/a";
  const abs = Math.abs(bytes);
  if (abs >= 1024 * 1024 * 1024) return `${fmtFixed(bytes / (1024 * 1024 * 1024), 2)} GiB`;
  if (abs >= 1024 * 1024) return `${fmtFixed(bytes / (1024 * 1024), 2)} MiB`;
  if (abs >= 1024) return `${fmtFixed(bytes / 1024, 2)} KiB`;
  return `${fmtFixed(bytes, 0)} B`;
}

/**
 * @param {number|null} value
 */
function fmtPct(value) {
  if (value == null || !Number.isFinite(value)) return "n/a";
  return `${fmtFixed(value * 100, 1)}%`;
}

/** @typedef {any} GpuTelemetrySnapshot */

export class DebugOverlay {
  /** @type {() => (GpuTelemetrySnapshot | null)} */
  _getSnapshot = () => null;
  _toggleKey = "F2";
  _updateIntervalMs = 250;
  /** @type {HTMLElement | null} */
  _parent = null;

  _visible = false;
  /** @type {HTMLDivElement | null} */
  _root = null;
  /** @type {number | null} */
  _interval = null;

  /** @type {(ev: KeyboardEvent) => void} */
  _onKeyDown = () => {};

  /**
   * @param {() => (GpuTelemetrySnapshot | null)} getSnapshot
   * @param {{
   *   toggleKey?: string,
   *   updateIntervalMs?: number,
   *   parent?: HTMLElement,
   * }=} opts
   */
  constructor(getSnapshot, opts = {}) {
    this._getSnapshot = getSnapshot;
    this._toggleKey = opts.toggleKey ?? this._toggleKey;
    this._updateIntervalMs = opts.updateIntervalMs ?? this._updateIntervalMs;
    this._parent = opts.parent ?? (typeof document !== "undefined" ? document.body : null);

    this._onKeyDown = (ev) => {
      if (ev.code === this._toggleKey && !ev.repeat) {
        ev.preventDefault();
        this.toggle();
      }
    };
  }

  attach() {
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

    window.addEventListener("keydown", this._onKeyDown, { capture: true });
  }

  detach() {
    if (!this._root) return;
    this.hide();
    window.removeEventListener("keydown", this._onKeyDown, { capture: true });
    this._root.remove();
    this._root = null;
  }

  show() {
    this.attach();
    if (!this._root) return;
    if (this._visible) return;

    this._visible = true;
    this._root.style.display = "block";

    this._interval = window.setInterval(() => this._render(), this._updateIntervalMs);
    this._render();
  }

  hide() {
    if (!this._root) return;
    if (!this._visible) return;

    this._visible = false;
    this._root.style.display = "none";

    if (this._interval != null) {
      window.clearInterval(this._interval);
      this._interval = null;
    }
  }

  toggle() {
    if (this._visible) this.hide();
    else this.show();
  }

  _render() {
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

    const lines = [];
    lines.push(`GPU Telemetry (toggle: ${this._toggleKey})`);
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
  const g = /** @type {any} */ (globalThis);
  if (!g.AeroDebugOverlay) {
    g.AeroDebugOverlay = { DebugOverlay };
  } else if (!g.AeroDebugOverlay.DebugOverlay) {
    g.AeroDebugOverlay.DebugOverlay = DebugOverlay;
  }
}
