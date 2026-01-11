export type StorageBenchBackend = "opfs" | "indexeddb";

export type StorageBenchApiMode = "sync_access_handle" | "async";

export interface StorageBenchOpts {
  backend?: StorageBenchBackend | "auto";
  /** Seed for deterministic random I/O patterns (when provided). */
  random_seed?: number;
  seq_total_mb?: number;
  seq_chunk_mb?: number;
  seq_runs?: number;
  warmup_mb?: number;
  random_ops?: number;
  random_runs?: number;
  random_space_mb?: number;
  include_random_write?: boolean;
  run_id?: string;
}

export interface StorageBenchThroughputRun {
  bytes: number;
  duration_ms: number;
  mb_per_s: number;
}

export interface StorageBenchThroughputSummary {
  runs: StorageBenchThroughputRun[];
  mean_mb_per_s: number;
  stdev_mb_per_s: number;
}

export interface StorageBenchLatencyRun {
  ops: number;
  block_bytes: number;
  min_ms: number;
  max_ms: number;
  mean_ms: number;
  stdev_ms: number;
  p50_ms: number;
  p95_ms: number;
}

export interface StorageBenchLatencySummary {
  runs: StorageBenchLatencyRun[];
  mean_p50_ms: number;
  mean_p95_ms: number;
  stdev_p50_ms: number;
  stdev_p95_ms: number;
}

export interface StorageBenchResult {
  version: 1;
  run_id: string;
  backend: StorageBenchBackend;
  api_mode: StorageBenchApiMode;
  config: Required<
    Pick<
      StorageBenchOpts,
      | "backend"
      | "random_seed"
      | "seq_total_mb"
      | "seq_chunk_mb"
      | "seq_runs"
      | "warmup_mb"
      | "random_ops"
      | "random_runs"
      | "random_space_mb"
      | "include_random_write"
    >
  >;
  sequential_write: StorageBenchThroughputSummary;
  sequential_read: StorageBenchThroughputSummary;
  random_read_4k: StorageBenchLatencySummary;
  random_write_4k?: StorageBenchLatencySummary;
  warnings?: string[];
}
