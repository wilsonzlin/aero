export type GuestCpuMode = "interpreter" | "jit_baseline" | "jit_opt";

export type GuestCpuBenchVariant =
  | "alu64"
  | "alu32"
  | "branch_pred64"
  | "branch_pred32"
  | "branch_unpred64"
  | "branch_unpred32"
  | "mem_seq64"
  | "mem_seq32"
  | "mem_stride64"
  | "mem_stride32"
  | "call_ret64"
  | "call_ret32";

export type GuestCpuBenchOpts = {
  variant: GuestCpuBenchVariant;
  mode: GuestCpuMode;

  /**
   * Time-budget mode. The harness repeats deterministic invocations using the
   * canonical `iters_per_run = 10_000` until the budget is reached.
   *
   * Mutually exclusive with `iters`.
   *
   * Must be a finite number > 0.
   */
  seconds?: number;

  /**
   * Fixed-iteration debug mode. The harness sets `iters_per_run = iters` and:
   * 1) runs an unmeasured "reference run" to derive `expected_checksum`
   * 2) runs a measured run, then asserts checksum determinism by comparing the
   *    measured checksum against the reference checksum.
   *
   * Mutually exclusive with `seconds`.
   *
   * Must be an integer in the u32 range (> 0).
   */
  iters?: number;
};

export type GuestCpuBenchRun = {
  variant: GuestCpuBenchVariant;
  mode: GuestCpuMode;

  iters_per_run: number;
  warmup_runs: number;
  measured_runs: number;

  /**
   * Expected checksum for this run.
   *
   * - `seconds` mode: the doc-provided canonical checksum for iters=10_000.
   * - `iters` mode: the checksum produced by the unmeasured reference run
   *   (same variant/mode/iters).
   */
  expected_checksum: string;
  /**
   * Checksum produced by the measured run (or, in `seconds` mode, the last
   * measured run).
   */
  observed_checksum: string;

  total_instructions: number;
  total_seconds: number;

  ips: number;
  mips: number;

  run_mips: number[];
  mips_mean: number;
  mips_stddev: number;
  mips_min: number;
  mips_max: number;
};

export type GuestCpuBenchPerfExport = {
  iters_per_run: number;
  warmup_runs: number;
  measured_runs: number;
  results: Array<{
    variant: GuestCpuBenchVariant;
    mode: GuestCpuMode;
    mips_mean: number;
    mips_stddev: number;
    mips_min: number;
    mips_max: number;
    expected_checksum: string;
    observed_checksum: string;
  }>;
};
