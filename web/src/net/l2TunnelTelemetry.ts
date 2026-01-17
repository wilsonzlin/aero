import type { LogLevel } from "../ipc/protocol";
import type { L2TunnelEvent } from "./l2Tunnel";
import {
  computeL2TunnelForwarderDropDeltas,
  formatL2TunnelForwarderLog,
  type L2TunnelForwarderConnectionState,
  type L2TunnelForwarderStats,
} from "./l2TunnelForwarder";

export type L2TunnelTelemetryOptions = Readonly<{
  getStats: () => L2TunnelForwarderStats;
  emitLog: (level: LogLevel, message: string) => void;
  intervalMs: number;
}>;

export class L2TunnelTelemetry {
  private readonly opts: L2TunnelTelemetryOptions;
  private connection: L2TunnelForwarderConnectionState = "closed";
  private lastStats: L2TunnelForwarderStats | null = null;
  private nextLogDeadlineMs = 0;

  constructor(opts: L2TunnelTelemetryOptions) {
    this.opts = opts;
  }

  get connectionState(): L2TunnelForwarderConnectionState {
    return this.connection;
  }

  onConnectInitiated(detail?: string): void {
    this.setConnectionState("connecting", { detail, level: "info" });
  }

  onStopped(detail?: string): void {
    this.setConnectionState("closed", { detail, level: "info" });
  }

  onTunnelEvent(ev: Exclude<L2TunnelEvent, { type: "frame" }>): void {
    if (ev.type === "open") {
      this.setConnectionState("open", { level: "info" });
      return;
    }
    if (ev.type === "close") {
      const suffix = ev.code === undefined ? "" : ` (code=${ev.code})`;
      this.setConnectionState("closed", { detail: suffix, level: "warn" });
      return;
    }
    if (ev.type === "error") {
      const message = ev.error instanceof Error ? ev.error.message : String(ev.error);
      // Errors can be non-fatal (e.g. malformed control messages) and the tunnel
      // may remain open. Surface the error without permanently switching the
      // connection state away from "open"/"connecting".
      this.nextLogDeadlineMs = 0;
      try {
        this.opts.emitLog("error", `l2: error: ${message}`);
      } catch {
        // Best-effort; never throw from the IO worker tick loop.
      }
      return;
    }
    // Ignore keepalive pongs; they're too noisy to surface in logs.
  }

  tick(nowMs: number): void {
    if (nowMs < this.nextLogDeadlineMs) return;
    this.nextLogDeadlineMs = nowMs + this.opts.intervalMs;

    let stats: L2TunnelForwarderStats;
    try {
      stats = this.opts.getStats();
    } catch {
      return;
    }

    const dropsSinceLast = computeL2TunnelForwarderDropDeltas(this.lastStats, stats);
    this.lastStats = stats;

    try {
      const anyDrops =
        dropsSinceLast.rxDroppedNetRxFull > 0 ||
        dropsSinceLast.rxDroppedPendingOverflow > 0 ||
        dropsSinceLast.txDroppedTunnelBackpressure > 0;
      this.opts.emitLog(anyDrops ? "warn" : "info", formatL2TunnelForwarderLog({ connection: this.connection, stats, dropsSinceLast }));
    } catch {
      // Best-effort; never throw from the IO worker tick loop.
    }
  }

  private setConnectionState(
    next: L2TunnelForwarderConnectionState,
    opts: { level: LogLevel; detail?: string } = { level: "info" },
  ): void {
    if (this.connection === next) return;
    this.connection = next;
    // Force an immediate periodic stats log after a state transition so the log stream
    // captures both the transition and the latest counters.
    this.nextLogDeadlineMs = 0;

    const detail = opts.detail ?? "";
    const suffix = detail.length > 0 ? (detail.startsWith(" ") ? detail : `: ${detail}`) : "";
    try {
      this.opts.emitLog(opts.level, `l2: ${next}${suffix}`);
    } catch {
      // Best-effort; never throw from the IO worker tick loop.
    }
  }
}
