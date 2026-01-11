import { describe, expect, it } from "vitest";

import { L2TunnelTelemetry } from "./l2TunnelTelemetry";
import type { L2TunnelForwarderStats } from "./l2TunnelForwarder";

function makeStats(overrides: Partial<L2TunnelForwarderStats> = {}): L2TunnelForwarderStats {
  return {
    running: true,
    txFrames: 0,
    txBytes: 0,
    txRingEmpty: 0,
    txDroppedNoTunnel: 0,
    txDroppedSendError: 0,
    txDroppedTunnelBackpressure: 0,
    rxFrames: 0,
    rxBytes: 0,
    rxRingFull: 0,
    rxPendingFrames: 0,
    rxPendingBytes: 0,
    rxDroppedNetRxFull: 0,
    rxDroppedPendingOverflow: 0,
    rxDroppedRingTooSmall: 0,
    rxDroppedWhileStopped: 0,
    ...overrides,
  };
}

describe("net/l2TunnelTelemetry", () => {
  it("emits periodic summary logs with drop deltas", () => {
    const logs: Array<{ level: string; message: string }> = [];
    let stats = makeStats({
      txFrames: 1,
      txBytes: 10,
      rxFrames: 2,
      rxBytes: 20,
      rxDroppedNetRxFull: 1,
      rxDroppedPendingOverflow: 1,
      txDroppedTunnelBackpressure: 1,
    });

    const telemetry = new L2TunnelTelemetry({
      intervalMs: 1000,
      getStats: () => stats,
      emitLog: (level, message) => logs.push({ level, message }),
    });

    telemetry.onConnectInitiated();
    expect(logs).toEqual([{ level: "info", message: "l2: connecting" }]);

    telemetry.tick(0);
    expect(logs).toHaveLength(2);
    expect(logs[1]!.level).toBe("info");
    expect(logs[1]!.message).toContain("l2: connecting");
    expect(logs[1]!.message).toContain("tx=1f/10B");
    expect(logs[1]!.message).toContain("rx=2f/20B");
    expect(logs[1]!.message).toContain("drop+{rx_full=1, pending=1, tx_bp=1}");

    stats = makeStats({
      txFrames: 5,
      txBytes: 50,
      rxFrames: 8,
      rxBytes: 80,
      rxDroppedNetRxFull: 3,
      rxDroppedPendingOverflow: 1,
      txDroppedTunnelBackpressure: 2,
    });

    telemetry.tick(999);
    expect(logs).toHaveLength(2);

    telemetry.tick(1000);
    expect(logs).toHaveLength(3);
    expect(logs[2]!.message).toContain("drop+{rx_full=2, pending=0, tx_bp=1}");
  });

  it("emits transition logs and resets the periodic schedule", () => {
    const logs: Array<{ level: string; message: string }> = [];
    const stats = makeStats({ rxDroppedNetRxFull: 7 });

    const telemetry = new L2TunnelTelemetry({
      intervalMs: 1000,
      getStats: () => stats,
      emitLog: (level, message) => logs.push({ level, message }),
    });

    telemetry.onConnectInitiated();
    telemetry.tick(0);
    expect(logs.map((l) => l.message)).toMatchObject(["l2: connecting", expect.stringContaining("l2: connecting")]);

    telemetry.tick(500);
    expect(logs).toHaveLength(2);

    telemetry.onTunnelEvent({ type: "open" });
    expect(logs[2]).toEqual({ level: "info", message: "l2: open" });

    // `open` transition should force an immediate summary rather than waiting for 1000ms.
    telemetry.tick(500);
    expect(logs).toHaveLength(4);
    expect(logs[3]!.level).toBe("info");
    expect(logs[3]!.message).toContain("l2: open");
  });

  it("clamps negative/NaN drop deltas in emitted logs", () => {
    const logs: Array<{ level: string; message: string }> = [];
    let stats = makeStats({
      rxDroppedNetRxFull: 10,
      rxDroppedPendingOverflow: 10,
      txDroppedTunnelBackpressure: 10,
    });

    const telemetry = new L2TunnelTelemetry({
      intervalMs: 1000,
      getStats: () => stats,
      emitLog: (level, message) => logs.push({ level, message }),
    });

    telemetry.onConnectInitiated();
    telemetry.tick(0);

    stats = makeStats({
      // Decrease the counters and introduce NaN; deltas must clamp to 0.
      rxDroppedNetRxFull: 5,
      rxDroppedPendingOverflow: Number.NaN,
      txDroppedTunnelBackpressure: 9,
    });

    telemetry.tick(1000);
    expect(logs.at(-1)!.message).toContain("drop+{rx_full=0, pending=0, tx_bp=0}");
  });
});

