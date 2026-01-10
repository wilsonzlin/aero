import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["web/tests/**/*.test.ts"],
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov"],
      reportsDirectory: "coverage",
      include: ["web/src/**/*.ts"],
      exclude: ["web/src/**/*.d.ts"]
    }
  }
});

