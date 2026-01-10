export interface StorageBenchThresholds {
  seq_write_mean_mb_per_s: number;
  seq_read_mean_mb_per_s: number;
  random_read_p95_ms: number;
}

export const defaultThresholds = {
  storage: {
    seq_write_mean_mb_per_s: 50,
    seq_read_mean_mb_per_s: 50,
    random_read_p95_ms: 10,
  } satisfies StorageBenchThresholds,
};

