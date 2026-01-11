import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Keep unit tests fast and deterministic; stub browser APIs explicitly.
    environment: "node",
    // These tests exercise blocking Atomics.wait() + Node worker_threads.
    // On newer Node releases, Vitest's default thread pool can interfere with
    // nested Worker scheduling / Atomics wakeups, causing flakes/timeouts.
    // Run tests in forked processes for deterministic cross-thread behavior.
    pool: "forks",
    include: ["web/tests/**/*.test.ts", "web/src/**/*.test.ts", "web/test/**/*.vitest.ts"],
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov"],
      reportsDirectory: "coverage",
      include: ["web/src/**/*.ts"],
      exclude: ["web/src/**/*.d.ts"],
    },
  },
});
