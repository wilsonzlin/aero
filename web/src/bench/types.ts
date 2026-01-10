export type MicrobenchMode = "fixedIters" | "timeBudget";

export interface MicrobenchSuiteOptions {
  mode?: MicrobenchMode;
  warmup?: boolean;

  // Used when `mode === "timeBudget"`.
  timeBudgetMs?: number;

  // Used when `mode === "fixedIters"`.
  integerAluIters?: number;
  branchyIters?: number;
  memcpyBytes?: number;
  memcpyIters?: number;
  hashBytes?: number;
  hashIters?: number;
}

export type MicrobenchCaseName = "integer_alu" | "branchy" | "memcpy" | "hash";

export interface MicrobenchCaseParamsV1 {
  iters: number;
  bytes?: number;
}

export type MicrobenchThroughputUnitV1 = "iters_per_sec" | "bytes_per_sec";

export interface MicrobenchThroughputV1 {
  unit: MicrobenchThroughputUnitV1;
  value: number;
}

export interface MicrobenchCaseResultV1 {
  name: MicrobenchCaseName;
  duration_ms: number;
  params: MicrobenchCaseParamsV1;
  checksum: string;
  throughput: MicrobenchThroughputV1;
}

export interface MicrobenchSuiteResultV1 {
  schema: "aero-microbench-suite-v1";
  started_ts_ms: number;
  finished_ts_ms: number;
  opts: Required<MicrobenchSuiteOptions>;
  cases: Record<MicrobenchCaseName, MicrobenchCaseResultV1>;
}
