import { afterEach, describe, expect, it, vi } from "vitest";

type MockRun = { checksumLo: number; retiredLo: number };

let mockRuns: MockRun[] = [];
let runCallCount = 0;

class FakeGuestCpuBenchHarness {
  payload_info(_variant: string): unknown {
    // Canonical checksum used by seconds mode.
    return { bitness: 32, expected_hi: 0, expected_lo: 0xaaaa_aaaa };
  }

  run_payload_once(_variant: string, _itersPerRun: number): unknown {
    const run = mockRuns[runCallCount++];
    if (!run) {
      throw new Error("fake harness: run_payload_once called more times than expected");
    }
    return {
      checksum_hi: 0,
      checksum_lo: run.checksumLo >>> 0,
      retired_hi: 0,
      retired_lo: run.retiredLo >>> 0,
    };
  }

  free(): void {}
}

vi.mock("../runtime/wasm_context", () => ({
  initWasmForContext: async () => ({
    api: { GuestCpuBenchHarness: FakeGuestCpuBenchHarness },
    variant: "single",
  }),
}));

afterEach(() => {
  mockRuns = [];
  runCallCount = 0;
  vi.restoreAllMocks();
  vi.resetModules();
});

describe("bench/guest_cpu_bench seconds mode semantics", () => {
  it("uses the canonical expected checksum from payload_info and runs warmups", async () => {
    // DEFAULT_WARMUP_RUNS (3) + measured_runs (1) = 4 invocations.
    mockRuns = [
      { checksumLo: 0xaaaa_aaaa, retiredLo: 10 }, // warmup 1
      { checksumLo: 0xaaaa_aaaa, retiredLo: 10 }, // warmup 2
      { checksumLo: 0xaaaa_aaaa, retiredLo: 10 }, // warmup 3
      { checksumLo: 0xaaaa_aaaa, retiredLo: 99 }, // measured
    ];

    // Ensure each run has a non-zero duration so the while loop terminates.
    let t = 0;
    vi.spyOn(performance, "now").mockImplementation(() => {
      t += 1;
      return t;
    });

    vi.resetModules();

    const { runGuestCpuBench } = await import("./guest_cpu_bench");
    const res = await runGuestCpuBench({
      variant: "alu32",
      mode: "interpreter",
      // Small time budget so we only do one measured run (warmups are always run).
      seconds: 0.000001,
    });

    expect(res.warmup_runs).toBe(3);
    expect(res.measured_runs).toBe(1);
    expect(res.expected_checksum).toBe("0xaaaaaaaa");
    expect(res.observed_checksum).toBe("0xaaaaaaaa");
    expect(res.total_instructions).toBe(99);
    expect(runCallCount).toBe(4);
  });

  it("throws on checksum mismatch in seconds mode", async () => {
    // Mismatch on warmup 2 to keep the test minimal.
    mockRuns = [
      { checksumLo: 0xaaaa_aaaa, retiredLo: 10 }, // warmup 1
      { checksumLo: 0xbbbb_bbbb, retiredLo: 10 }, // warmup 2 (mismatch)
    ];

    let t = 0;
    vi.spyOn(performance, "now").mockImplementation(() => {
      t += 1;
      return t;
    });

    vi.resetModules();

    const { runGuestCpuBench } = await import("./guest_cpu_bench");
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        seconds: 0.000001,
      }),
    ).rejects.toThrow(/checksum mismatch/i);
  });

  it("fails fast if the timer source does not advance (hard timeout guard)", async () => {
    // DEFAULT_WARMUP_RUNS (3) + first measured run = 4 invocations before the hard-timeout trips.
    mockRuns = [
      { checksumLo: 0xaaaa_aaaa, retiredLo: 10 }, // warmup 1
      { checksumLo: 0xaaaa_aaaa, retiredLo: 10 }, // warmup 2
      { checksumLo: 0xaaaa_aaaa, retiredLo: 10 }, // warmup 3
      { checksumLo: 0xaaaa_aaaa, retiredLo: 99 }, // measured run
    ];

    // Simulate a pathological environment where `performance.now()` always returns the same value.
    vi.spyOn(performance, "now").mockImplementation(() => 0);

    // But keep wall-clock moving so the hard-timeout check triggers immediately.
    let wall = 0;
    vi.spyOn(Date, "now").mockImplementation(() => {
      wall += 2000;
      return wall;
    });

    vi.resetModules();

    const { runGuestCpuBench } = await import("./guest_cpu_bench");
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        // Small time budget so the hard-timeout threshold is the minimum (1000ms).
        seconds: 0.000001,
      }),
    ).rejects.toThrow(/hard timeout/i);
    expect(runCallCount).toBe(4);
  });
});
