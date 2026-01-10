import { defineConfig, mergeConfig } from "vitest/config";

import viteConfig from "./vite.config";

export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      // Keep unit tests fast and deterministic; stub browser APIs explicitly.
      environment: "node",
      include: ["src/**/*.test.ts"],
      coverage: {
        provider: "v8",
        reportsDirectory: "coverage",
        reporter: ["text", "lcov"],
        include: ["src/**/*.{ts,tsx}"],
        exclude: [
          "**/*.d.ts",
          "**/node_modules/**",
          "**/coverage/**",
          "**/dist/**",
          "**/build/**",
          "**/.vite/**",
          // wasm-bindgen / wasm-pack output typically lives in `pkg/` and uses `*_bg.*`.
          "**/pkg/**",
          "**/*_bg.js",
          "**/*_bg.wasm",
        ],
      },
    },
  }),
);
