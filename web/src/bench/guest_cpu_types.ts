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
  seconds?: number;
  iters?: number;
};

export type GuestCpuBenchRun = {
  variant: GuestCpuBenchVariant;
  mode: GuestCpuMode;

  iters_per_run: number;
  warmup_runs: number;
  measured_runs: number;

  expected_checksum: string;
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

