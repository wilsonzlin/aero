import { afterEach, describe, expect, it, vi } from "vitest";

type MockRun = { checksumLo: number; retiredLo: number };

let mockRuns: MockRun[] = [];
let runCallCount = 0;

class FakeGuestCpuBenchHarness {
  payload_info(_variant: string): unknown {
    // Canonical checksum from docs (alu32 @ 10k iters) is 0x30aae0b8, but we
    // intentionally return a different value here so tests can assert that
    // `iters` mode does *not* use the canonical expected checksum.
    return { bitness: 32, expected_hi: 0, expected_lo: 0xdead_beef };
  }

  run_payload_once(_variant: string, _itersPerRun: number): unknown {
    const run = mockRuns[runCallCount++];
    if (!run) {
      throw new Error("fake harness: run_payload_once called more times than expected");
    }
    return {
      checksum_hi: 0,
      checksum_lo: run.checksumLo >>> 0,
      // Match the field names emitted by the Rust WASM harness.
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
  vi.resetModules();
  vi.clearAllMocks();
});

describe("bench/guest_cpu_bench iters mode semantics", () => {
  it("derives expected_checksum from an unmeasured reference run and uses measured run counters", async () => {
    mockRuns = [
      { checksumLo: 0x1234_5678, retiredLo: 111 }, // reference run
      { checksumLo: 0x1234_5678, retiredLo: 222 }, // measured run
    ];

    // Ensure we load `guest_cpu_bench` fresh with our wasm_context mock even if
    // another test file imported it earlier in this worker process.
    vi.resetModules();

    const { runGuestCpuBench } = await import("./guest_cpu_bench");
    const res = await runGuestCpuBench({
      variant: "alu32",
      mode: "interpreter",
      iters: 1234,
    });

    expect(res.iters_per_run).toBe(1234);
    expect(res.warmup_runs).toBe(0);
    expect(res.measured_runs).toBe(1);

    // `iters` mode must not return the canonical checksum.
    expect(res.expected_checksum).toBe("0x12345678");
    expect(res.observed_checksum).toBe("0x12345678");

    // Totals come from the measured run only.
    expect(res.total_instructions).toBe(222);

    // Ensure the harness was invoked exactly twice (reference + measured).
    expect(runCallCount).toBe(2);
  });

  it("throws if the measured run checksum differs from the reference run checksum", async () => {
    mockRuns = [
      { checksumLo: 0x1111_1111, retiredLo: 1 }, // reference
      { checksumLo: 0x2222_2222, retiredLo: 1 }, // measured
    ];

    vi.resetModules();

    const { runGuestCpuBench } = await import("./guest_cpu_bench");
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        iters: 1234,
      }),
    ).rejects.toThrow(/determinism/i);
  });
});
