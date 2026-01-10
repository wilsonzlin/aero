let benchmarks: Record<string, unknown> = {};

export function clearBenchmarks(): void {
  benchmarks = {};
}

export function setBenchmark(name: string, value: unknown): void {
  benchmarks[name] = value;
}

export function getBenchmarksSnapshot(): Record<string, unknown> {
  return { ...benchmarks };
}

