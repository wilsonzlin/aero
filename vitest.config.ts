import { defineConfig } from "vitest/config";

const nodeMajor = (() => {
  const major = Number.parseInt(process.versions.node.split(".", 1)[0] ?? "", 10);
  return Number.isFinite(major) ? major : 0;
})();

const vitestMaxForks = (() => {
  // `vitest` uses Tinypool + `child_process.fork()` when `pool: "forks"` is enabled below.
  //
  // In sandboxed environments with tight process/thread limits, newer Node majors can fail to
  // spawn or cleanly tear down large numbers of forks. Keep CI (Node 22.x) reasonably parallel,
  // but cap newer majors more aggressively so unit tests remain stable for contributors /
  // hermetic runners.
  if (nodeMajor >= 25) return 2;
  if (nodeMajor >= 23) return 4;
  return 8;
})();

export default defineConfig({
  test: {
    // Keep unit tests fast and deterministic; stub browser APIs explicitly.
    environment: "node",
    // These tests exercise blocking Atomics.wait() + Node worker_threads.
    // Vitest's default thread pool can interfere with nested Worker scheduling /
    // Atomics wakeups. Run tests in forked processes for deterministic behavior.
    pool: "forks",
    // Vitest defaults its pool size based on `os.cpus()`, which can be extremely large in
    // sandbox environments (e.g. 192 vCPUs). Spawning that many Node processes can exhaust
    // memory / pthread resources and crash or hang workers mid-run.
    //
    // Cap the fork count so `vitest run` remains stable even when the host reports very high
    // core counts.
    poolOptions: {
      forks: {
        minForks: 1,
        maxForks: vitestMaxForks,
      },
    },
    include: ["web/src/**/*.test.ts", "web/test/**/*.vitest.ts", "services/**/test/**/*.test.ts"],
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov"],
      reportsDirectory: "coverage",
      include: ["web/src/**/*.ts", "services/**/src/**/*.ts"],
      exclude: ["**/*.d.ts"],
    },
  },
});
