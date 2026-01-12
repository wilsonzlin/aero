import { RECORD_ALIGN, alignUp } from "../ipc/layout";
import type { RingBuffer } from "../ipc/ring_buffer";
import type { L2TunnelClient, L2TunnelEvent, L2TunnelSink } from "./l2Tunnel";

export type L2TunnelForwarderConnectionState = "open" | "closed" | "connecting" | "error";

export type L2TunnelForwarderDropDeltas = Readonly<{
  rxDroppedNetRxFull: number;
  rxDroppedPendingOverflow: number;
  txDroppedTunnelBackpressure: number;
}>;

export type L2TunnelForwarderOptions = Readonly<{
  /**
   * Maximum number of guest->host frames to drain from the NET_TX ring per `tick()`.
   * Defaults to unlimited.
   */
  maxFramesPerTick?: number;

  /**
   * Maximum number of bytes allowed to accumulate in the pending inbound queue
   * (host->guest) when NET_RX is full.
   *
   * When exceeded, new inbound frames are dropped.
   *
   * Setting this to `0` disables buffering and instead drops frames immediately
   * when NET_RX is full (counted as `rxDroppedNetRxFull`).
   *
   * Default: 8 MiB
   */
  maxPendingRxBytes?: number;

  /**
   * Optional hook for non-frame tunnel events.
   *
   * Note: this is invoked from the tunnel client's event handler; it MUST NOT
   * throw.
   */
  onTunnelEvent?: (ev: Exclude<L2TunnelEvent, { type: "frame" }>) => void;

  /**
   * Optional hook invoked for each Ethernet frame observed by the forwarder.
   *
   * Frames are reported best-effort and the hook must never throw.
   *
   * - `guest_tx`: The guest produced a frame and it was drained from `NET_TX`.
   *   This is invoked immediately when the frame is read from the ring buffer
   *   (before forwarding decisions), so captures may include frames that are
   *   later dropped due to missing tunnel/backpressure.
   * - `guest_rx`: The forwarder received an inbound tunnel frame. This is
   *   invoked at the start of inbound processing, before checking `running` or
   *   `NET_RX` capacity.
   *
   * Note: `guest_tx` frames come from `RingBuffer.consumeNext` and may be backed
   * by the ring's underlying `SharedArrayBuffer`; callers MUST copy if they
   * retain `frame` beyond the hook invocation.
   */
  onFrame?: (ev: { direction: "guest_tx" | "guest_rx"; frame: Uint8Array }) => void;
}>;

export type L2TunnelForwarderStats = Readonly<{
  running: boolean;

  /** Frames successfully forwarded from NET_TX → tunnel. */
  txFrames: number;
  /** Bytes successfully forwarded from NET_TX → tunnel. */
  txBytes: number;
  /** Number of times `tick()` observed NET_TX empty. */
  txRingEmpty: number;
  /** Frames dropped because the forwarder is stopped or has no active tunnel. */
  txDroppedNoTunnel: number;
  /** Frames dropped because the tunnel transport refused/errored on send. */
  txDroppedSendError: number;
  /** Alias for `txDroppedSendError` (kept for log/telemetry naming). */
  txDroppedTunnelBackpressure: number;

  /** Frames successfully forwarded from tunnel → NET_RX. */
  rxFrames: number;
  /** Bytes successfully forwarded from tunnel → NET_RX. */
  rxBytes: number;
  /** Number of times an inbound frame could not be pushed to NET_RX (ring full). */
  rxRingFull: number;
  /** Pending tunnel→guest frames buffered in JS awaiting NET_RX space. */
  rxPendingFrames: number;
  /** Pending tunnel→guest bytes buffered in JS awaiting NET_RX space. */
  rxPendingBytes: number;
  /** Frames dropped because NET_RX is full and buffering is disabled (maxPendingRxBytes=0). */
  rxDroppedNetRxFull: number;
  /** Frames dropped because the pending RX buffer overflowed. */
  rxDroppedPendingOverflow: number;
  /** Frames dropped because the NET_RX ring is too small to ever fit them. */
  rxDroppedRingTooSmall: number;
  /** Frames dropped because the forwarder is stopped. */
  rxDroppedWhileStopped: number;
}>;

const DEFAULT_MAX_PENDING_RX_BYTES = 8 * 1024 * 1024;

function validateNonNegativeInt(name: string, value: number): void {
  if (!Number.isInteger(value) || value < 0) {
    throw new RangeError(`${name} must be a non-negative integer (got ${value})`);
  }
}

function clampNonNegativeDelta(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return value > 0 ? value : 0;
}

export function computeL2TunnelForwarderDropDeltas(
  prev: L2TunnelForwarderStats | null,
  next: L2TunnelForwarderStats,
): L2TunnelForwarderDropDeltas {
  if (!prev) {
    return {
      rxDroppedNetRxFull: next.rxDroppedNetRxFull,
      rxDroppedPendingOverflow: next.rxDroppedPendingOverflow,
      txDroppedTunnelBackpressure: next.txDroppedTunnelBackpressure,
    };
  }
  return {
    rxDroppedNetRxFull: clampNonNegativeDelta(next.rxDroppedNetRxFull - prev.rxDroppedNetRxFull),
    rxDroppedPendingOverflow: clampNonNegativeDelta(next.rxDroppedPendingOverflow - prev.rxDroppedPendingOverflow),
    txDroppedTunnelBackpressure: clampNonNegativeDelta(next.txDroppedTunnelBackpressure - prev.txDroppedTunnelBackpressure),
  };
}

export function formatL2TunnelForwarderLog(args: {
  connection: L2TunnelForwarderConnectionState;
  stats: L2TunnelForwarderStats;
  dropsSinceLast: L2TunnelForwarderDropDeltas;
}): string {
  const { connection, stats, dropsSinceLast } = args;
  return [
    `l2: ${connection}`,
    `tx=${stats.txFrames}f/${stats.txBytes}B`,
    `rx=${stats.rxFrames}f/${stats.rxBytes}B`,
    `drop+{rx_full=${dropsSinceLast.rxDroppedNetRxFull}, pending=${dropsSinceLast.rxDroppedPendingOverflow}, tx_bp=${dropsSinceLast.txDroppedTunnelBackpressure}}`,
    `pending=${stats.rxPendingFrames}f/${stats.rxPendingBytes}B`,
  ].join(" ");
}

export class L2TunnelForwarder {
  private readonly netTx: RingBuffer;
  private readonly netRx: RingBuffer;
  private readonly maxFramesPerTick: number;
  private readonly maxPendingRxBytes: number;
  private readonly onTunnelEvent: ((ev: Exclude<L2TunnelEvent, { type: "frame" }>) => void) | null;
  private onFrame: ((ev: { direction: "guest_tx" | "guest_rx"; frame: Uint8Array }) => void) | null;

  private tunnel: L2TunnelClient | null = null;
  private running = false;

  private pendingRx: Uint8Array[] = [];
  private pendingRxHead = 0;
  private pendingRxBytes = 0;

  private txFrames = 0;
  private txBytes = 0;
  private txRingEmpty = 0;
  private txDroppedNoTunnel = 0;
  private txDroppedSendError = 0;

  private rxFrames = 0;
  private rxBytes = 0;
  private rxRingFull = 0;
  private rxDroppedNetRxFull = 0;
  private rxDroppedPendingOverflow = 0;
  private rxDroppedRingTooSmall = 0;
  private rxDroppedWhileStopped = 0;

  readonly sink: L2TunnelSink;

  constructor(
    netTx: RingBuffer,
    netRx: RingBuffer,
    opts: L2TunnelForwarderOptions = {},
  ) {
    this.netTx = netTx;
    this.netRx = netRx;
    const maxFramesPerTick = opts.maxFramesPerTick ?? Number.POSITIVE_INFINITY;
    const maxPendingRxBytes = opts.maxPendingRxBytes ?? DEFAULT_MAX_PENDING_RX_BYTES;
    validateNonNegativeInt("maxFramesPerTick", maxFramesPerTick === Number.POSITIVE_INFINITY ? 0 : maxFramesPerTick);
    validateNonNegativeInt("maxPendingRxBytes", maxPendingRxBytes);

    this.maxFramesPerTick = maxFramesPerTick;
    this.maxPendingRxBytes = maxPendingRxBytes;
    this.onTunnelEvent = opts.onTunnelEvent ?? null;
    this.onFrame = opts.onFrame ?? null;

    this.sink = (ev) => {
      try {
        this.handleTunnelEvent(ev);
      } catch {
        // Never throw from an event handler: this would bubble into the
        // WebSocket/DataChannel callback and can tear down the session.
      }
    };
  }

  setOnFrame(onFrame: ((ev: { direction: "guest_tx" | "guest_rx"; frame: Uint8Array }) => void) | null): void {
    this.onFrame = onFrame;
  }

  setTunnel(tunnel: L2TunnelClient | null): void {
    if (this.tunnel === tunnel) return;

    // The forwarder owns the tunnel lifecycle to avoid leaking keepalive timers
    // in long-lived workers.
    const prev = this.tunnel;
    this.tunnel = tunnel;
    try {
      prev?.close();
    } catch {
      // Best-effort cleanup.
    }

    if (this.running && this.tunnel) {
      try {
        this.tunnel.connect();
      } catch {
        // Ignore; the tunnel will surface errors via its sink.
      }
    }
  }

  start(): void {
    this.running = true;
    if (this.tunnel) {
      try {
        this.tunnel.connect();
      } catch {
        // Ignore; the tunnel will surface errors via its sink.
      }
    }
  }

  stop(): void {
    this.running = false;

    const tunnel = this.tunnel;
    this.tunnel = null;
    try {
      tunnel?.close();
    } catch {
      // Best-effort cleanup.
    }

    this.clearPendingRx();
  }

  /**
   * Pump both directions once.
   *
   * Alias for `tick()` so worker-side polling loops can call `pump()` without
   * caring about the exact method name.
   */
  pump(): void {
    this.tick();
  }

  tick(): void {
    this.flushPendingRx();
    this.drainNetTx();
  }

  stats(): L2TunnelForwarderStats {
    const pendingFrames = this.pendingRx.length - this.pendingRxHead;
    return {
      running: this.running,

      txFrames: this.txFrames,
      txBytes: this.txBytes,
      txRingEmpty: this.txRingEmpty,
      txDroppedNoTunnel: this.txDroppedNoTunnel,
      txDroppedSendError: this.txDroppedSendError,
      txDroppedTunnelBackpressure: this.txDroppedSendError,

      rxFrames: this.rxFrames,
      rxBytes: this.rxBytes,
      rxRingFull: this.rxRingFull,
      rxPendingFrames: pendingFrames,
      rxPendingBytes: this.pendingRxBytes,
      rxDroppedNetRxFull: this.rxDroppedNetRxFull,
      rxDroppedPendingOverflow: this.rxDroppedPendingOverflow,
      rxDroppedRingTooSmall: this.rxDroppedRingTooSmall,
      rxDroppedWhileStopped: this.rxDroppedWhileStopped,
    };
  }

  private recordSizeForPayload(payloadByteLength: number): number {
    // `RingBuffer` record layout is `len(u32) | payload`, padded to RECORD_ALIGN.
    return alignUp(4 + payloadByteLength, RECORD_ALIGN);
  }

  private clearPendingRx(): void {
    this.pendingRx = [];
    this.pendingRxHead = 0;
    this.pendingRxBytes = 0;
  }

  private flushPendingRx(): void {
    const ring = this.netRx;

    while (this.pendingRxHead < this.pendingRx.length) {
      const frame = this.pendingRx[this.pendingRxHead]!;
      if (!ring.tryPush(frame)) break;

      this.pendingRxHead += 1;
      this.pendingRxBytes -= frame.byteLength;
      this.rxFrames += 1;
      this.rxBytes += frame.byteLength;
    }

    // Reclaim memory once we've drained part (or all) of the queue.
    if (this.pendingRxHead > 0) {
      this.pendingRx = this.pendingRx.slice(this.pendingRxHead);
      this.pendingRxHead = 0;
    }
  }

  private drainNetTx(): void {
    let drained = 0;
    let stopOnBackpressure = false;
    while (drained < this.maxFramesPerTick && !stopOnBackpressure) {
      const didConsume = this.netTx.consumeNext((frame) => {
        drained += 1;

        const onFrame = this.onFrame;
        if (onFrame) {
          try {
            onFrame({ direction: "guest_tx", frame });
          } catch {
            // Caller hooks must never crash the worker loop.
          }
        }

        const tunnel = this.tunnel;
        if (!this.running || !tunnel) {
          this.txDroppedNoTunnel += 1;
          return;
        }

        let ok = true;
        try {
          const res = tunnel.sendFrame(frame);
          if (res === false) ok = false;
        } catch {
          ok = false;
        }

        if (!ok) {
          this.txDroppedSendError += 1;
          // Backpressure is usually correlated across subsequent frames; stop
          // draining NET_TX so we don't convert an ephemeral stall into a burst of drops.
          stopOnBackpressure = true;
          return;
        }

        this.txFrames += 1;
        this.txBytes += frame.byteLength;
      });

      if (!didConsume) {
        this.txRingEmpty += 1;
        return;
      }
    }
  }

  private handleTunnelEvent(ev: L2TunnelEvent): void {
    if (ev.type === "frame") {
      this.handleInboundFrame(ev.frame);
      return;
    }

    const hook = this.onTunnelEvent;
    if (!hook) return;
    try {
      hook(ev);
    } catch {
      // Caller hooks must never crash the transport event handler.
    }
  }

  private handleInboundFrame(frame: Uint8Array): void {
    const onFrame = this.onFrame;
    if (onFrame) {
      try {
        onFrame({ direction: "guest_rx", frame });
      } catch {
        // Caller hooks must never crash the transport event handler.
      }
    }

    if (!this.running) {
      this.rxDroppedWhileStopped += 1;
      return;
    }

    const recordSize = this.recordSizeForPayload(frame.byteLength);
    if (recordSize > this.netRx.capacityBytes()) {
      this.rxDroppedRingTooSmall += 1;
      return;
    }

    if (this.netRx.tryPush(frame)) {
      this.rxFrames += 1;
      this.rxBytes += frame.byteLength;
      return;
    }

    this.rxRingFull += 1;

    // If buffering is disabled, treat ring-full as a hard drop.
    if (this.maxPendingRxBytes === 0) {
      this.rxDroppedNetRxFull += 1;
      return;
    }

    if (this.pendingRxBytes + frame.byteLength > this.maxPendingRxBytes) {
      this.rxDroppedPendingOverflow += 1;
      return;
    }

    this.pendingRx.push(frame);
    this.pendingRxBytes += frame.byteLength;
  }
}
