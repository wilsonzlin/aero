import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Keep unit tests fast and deterministic; stub browser APIs explicitly.
    environment: "node",
    // These tests exercise blocking Atomics.wait() + Node worker_threads.
    // Vitest's default thread pool can interfere with nested Worker scheduling /
    // Atomics wakeups. Run tests in forked processes for deterministic behavior.
    pool: "forks",
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

