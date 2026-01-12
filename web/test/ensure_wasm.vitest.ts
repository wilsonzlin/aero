import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("node:child_process", () => ({ spawnSync: vi.fn() }));
vi.mock("node:fs", () => ({ existsSync: vi.fn() }));

function normalizePath(p: unknown): string {
  return String(p).replaceAll("\\", "/");
}

afterEach(() => {
  vi.clearAllMocks();
  vi.resetModules();
});

describe("web/scripts/ensure_wasm.mjs", () => {
  it("builds missing aero-jit-wasm outputs when core + gpu outputs exist", async () => {
    const { existsSync } = await import("node:fs");
    const { spawnSync } = await import("node:child_process");
    const existsSyncMock = vi.mocked(existsSync);
    const spawnSyncMock = vi.mocked(spawnSync);

    let builtJit = false;

    existsSyncMock.mockImplementation((file) => {
      const p = normalizePath(file);

      // Pretend the JIT crate exists.
      if (p.endsWith("/crates/aero-jit-wasm/Cargo.toml")) return true;

      // Core + GPU outputs already exist.
      if (p.includes("/web/src/wasm/pkg-single/") && (p.endsWith("/aero_wasm.js") || p.endsWith("/aero_wasm_bg.wasm"))) {
        return true;
      }
      if (
        p.includes("/web/src/wasm/pkg-single-gpu/") &&
        (p.endsWith("/aero_gpu_wasm.js") || p.endsWith("/aero_gpu_wasm_bg.wasm"))
      ) {
        return true;
      }

      // JIT outputs are missing until the build step runs.
      if (
        p.includes("/web/src/wasm/pkg-jit-single/") &&
        (p.endsWith("/aero_jit_wasm.js") || p.endsWith("/aero_jit_wasm_bg.wasm"))
      ) {
        return builtJit;
      }

      return false;
    });

    spawnSyncMock.mockImplementation(() => {
      builtJit = true;
      return { status: 0 } as unknown as ReturnType<typeof spawnSync>;
    });

    const { ensureVariant } = await import("../scripts/ensure_wasm.mjs");
    ensureVariant("single");

    expect(spawnSyncMock).toHaveBeenCalledTimes(1);
    const [cmd, args] = spawnSyncMock.mock.calls[0]!;
    expect(cmd).toBe("node");
    expect(Array.isArray(args)).toBe(true);
    const argv = args as string[];
    expect(argv[1]).toBe("single");
    const idx = argv.indexOf("--packages");
    expect(idx).toBeGreaterThanOrEqual(0);
    expect(argv[idx + 1]).toBe("jit");
  });

  it("builds missing aero-jit-wasm outputs for the threaded variant when core + gpu outputs exist", async () => {
    const { existsSync } = await import("node:fs");
    const { spawnSync } = await import("node:child_process");
    const existsSyncMock = vi.mocked(existsSync);
    const spawnSyncMock = vi.mocked(spawnSync);

    let builtJit = false;

    existsSyncMock.mockImplementation((file) => {
      const p = normalizePath(file);

      // Pretend the JIT crate exists.
      if (p.endsWith("/crates/aero-jit-wasm/Cargo.toml")) return true;

      // Core + GPU outputs already exist for the threaded build.
      if (
        p.includes("/web/src/wasm/pkg-threaded/") &&
        (p.endsWith("/aero_wasm.js") || p.endsWith("/aero_wasm_bg.wasm"))
      ) {
        return true;
      }
      if (
        p.includes("/web/src/wasm/pkg-threaded-gpu/") &&
        (p.endsWith("/aero_gpu_wasm.js") || p.endsWith("/aero_gpu_wasm_bg.wasm"))
      ) {
        return true;
      }

      // JIT outputs are missing until the build step runs.
      if (
        p.includes("/web/src/wasm/pkg-jit-threaded/") &&
        (p.endsWith("/aero_jit_wasm.js") || p.endsWith("/aero_jit_wasm_bg.wasm"))
      ) {
        return builtJit;
      }

      return false;
    });

    spawnSyncMock.mockImplementation(() => {
      builtJit = true;
      return { status: 0 } as unknown as ReturnType<typeof spawnSync>;
    });

    const { ensureVariant } = await import("../scripts/ensure_wasm.mjs");
    ensureVariant("threaded");

    expect(spawnSyncMock).toHaveBeenCalledTimes(1);
    const [cmd, args] = spawnSyncMock.mock.calls[0]!;
    expect(cmd).toBe("node");
    expect(Array.isArray(args)).toBe(true);
    const argv = args as string[];
    expect(argv[1]).toBe("threaded");
    const idx = argv.indexOf("--packages");
    expect(idx).toBeGreaterThanOrEqual(0);
    expect(argv[idx + 1]).toBe("jit");
  });

  it("does not rebuild when all expected outputs already exist", async () => {
    const { existsSync } = await import("node:fs");
    const { spawnSync } = await import("node:child_process");
    const existsSyncMock = vi.mocked(existsSync);
    const spawnSyncMock = vi.mocked(spawnSync);

    existsSyncMock.mockImplementation((file) => {
      const p = normalizePath(file);
      if (p.endsWith("/crates/aero-jit-wasm/Cargo.toml")) return true;
      if (p.includes("/web/src/wasm/pkg-single/") && (p.endsWith("/aero_wasm.js") || p.endsWith("/aero_wasm_bg.wasm"))) {
        return true;
      }
      if (
        p.includes("/web/src/wasm/pkg-single-gpu/") &&
        (p.endsWith("/aero_gpu_wasm.js") || p.endsWith("/aero_gpu_wasm_bg.wasm"))
      ) {
        return true;
      }
      if (
        p.includes("/web/src/wasm/pkg-jit-single/") &&
        (p.endsWith("/aero_jit_wasm.js") || p.endsWith("/aero_jit_wasm_bg.wasm"))
      ) {
        return true;
      }
      return false;
    });

    const { ensureVariant } = await import("../scripts/ensure_wasm.mjs");
    ensureVariant("single");

    expect(spawnSyncMock).not.toHaveBeenCalled();
  });

  it("does not require pkg-jit-* outputs when the aero-jit-wasm crate is absent", async () => {
    const { existsSync } = await import("node:fs");
    const { spawnSync } = await import("node:child_process");
    const existsSyncMock = vi.mocked(existsSync);
    const spawnSyncMock = vi.mocked(spawnSync);

    existsSyncMock.mockImplementation((file) => {
      const p = normalizePath(file);

      // JIT crate missing.
      if (p.endsWith("/crates/aero-jit-wasm/Cargo.toml")) return false;

      // Core + GPU outputs exist.
      if (p.includes("/web/src/wasm/pkg-single/") && (p.endsWith("/aero_wasm.js") || p.endsWith("/aero_wasm_bg.wasm"))) {
        return true;
      }
      if (
        p.includes("/web/src/wasm/pkg-single-gpu/") &&
        (p.endsWith("/aero_gpu_wasm.js") || p.endsWith("/aero_gpu_wasm_bg.wasm"))
      ) {
        return true;
      }

      // Everything else missing (including pkg-jit-*).
      return false;
    });

    const { ensureVariant } = await import("../scripts/ensure_wasm.mjs");
    ensureVariant("single");

    // Since the JIT crate is absent, ensure_wasm should not try to build just because pkg-jit-single is missing.
    expect(spawnSyncMock).not.toHaveBeenCalled();
  });

  it("ensureAll builds only the missing variant(s)", async () => {
    const { existsSync } = await import("node:fs");
    const { spawnSync } = await import("node:child_process");
    const existsSyncMock = vi.mocked(existsSync);
    const spawnSyncMock = vi.mocked(spawnSync);

    let builtThreaded = false;

    existsSyncMock.mockImplementation((file) => {
      const p = normalizePath(file);

      // Pretend the JIT crate exists.
      if (p.endsWith("/crates/aero-jit-wasm/Cargo.toml")) return true;

      // Single variant already fully built (core + gpu + jit).
      if (p.includes("/web/src/wasm/pkg-single/") && (p.endsWith("/aero_wasm.js") || p.endsWith("/aero_wasm_bg.wasm"))) {
        return true;
      }
      if (
        p.includes("/web/src/wasm/pkg-single-gpu/") &&
        (p.endsWith("/aero_gpu_wasm.js") || p.endsWith("/aero_gpu_wasm_bg.wasm"))
      ) {
        return true;
      }
      if (
        p.includes("/web/src/wasm/pkg-jit-single/") &&
        (p.endsWith("/aero_jit_wasm.js") || p.endsWith("/aero_jit_wasm_bg.wasm"))
      ) {
        return true;
      }

      // Threaded variant is missing until we "build" it.
      if (
        p.includes("/web/src/wasm/pkg-threaded/") &&
        (p.endsWith("/aero_wasm.js") || p.endsWith("/aero_wasm_bg.wasm"))
      ) {
        return builtThreaded;
      }
      if (
        p.includes("/web/src/wasm/pkg-threaded-gpu/") &&
        (p.endsWith("/aero_gpu_wasm.js") || p.endsWith("/aero_gpu_wasm_bg.wasm"))
      ) {
        return builtThreaded;
      }
      if (
        p.includes("/web/src/wasm/pkg-jit-threaded/") &&
        (p.endsWith("/aero_jit_wasm.js") || p.endsWith("/aero_jit_wasm_bg.wasm"))
      ) {
        return builtThreaded;
      }

      return false;
    });

    spawnSyncMock.mockImplementation((_cmd, args) => {
      const variant = Array.isArray(args) ? String(args[1]) : "";
      if (variant === "threaded") {
        builtThreaded = true;
      }
      return { status: 0 } as unknown as ReturnType<typeof spawnSync>;
    });

    const { ensureAll } = await import("../scripts/ensure_wasm.mjs");
    ensureAll();

    // Only threaded should be built.
    expect(spawnSyncMock).toHaveBeenCalledTimes(1);
    const [_cmd, args] = spawnSyncMock.mock.calls[0]!;
    expect(Array.isArray(args)).toBe(true);
    const argv = args as string[];
    expect(argv[1]).toBe("threaded");
    const idx = argv.indexOf("--packages");
    expect(idx).toBeGreaterThanOrEqual(0);
    expect(argv[idx + 1]).toBe("core,gpu,jit");
  });

  it("fails if build succeeds but expected outputs are still missing", async () => {
    const { existsSync } = await import("node:fs");
    const { spawnSync } = await import("node:child_process");
    const existsSyncMock = vi.mocked(existsSync);
    const spawnSyncMock = vi.mocked(spawnSync);

    existsSyncMock.mockImplementation((file) => {
      const p = normalizePath(file);

      // Pretend the JIT crate exists.
      if (p.endsWith("/crates/aero-jit-wasm/Cargo.toml")) return true;

      // Core + GPU outputs already exist.
      if (p.includes("/web/src/wasm/pkg-single/") && (p.endsWith("/aero_wasm.js") || p.endsWith("/aero_wasm_bg.wasm"))) {
        return true;
      }
      if (
        p.includes("/web/src/wasm/pkg-single-gpu/") &&
        (p.endsWith("/aero_gpu_wasm.js") || p.endsWith("/aero_gpu_wasm_bg.wasm"))
      ) {
        return true;
      }

      // JIT outputs never materialize (even after the build step).
      if (
        p.includes("/web/src/wasm/pkg-jit-single/") &&
        (p.endsWith("/aero_jit_wasm.js") || p.endsWith("/aero_jit_wasm_bg.wasm"))
      ) {
        return false;
      }

      return false;
    });

    spawnSyncMock.mockImplementation(() => {
      return { status: 0 } as unknown as ReturnType<typeof spawnSync>;
    });

    const { ensureVariant } = await import("../scripts/ensure_wasm.mjs");
    expect(() => ensureVariant("single")).toThrowError(/expected wasm-pack outputs are still missing/);
    expect(spawnSyncMock).toHaveBeenCalledTimes(1);
  });

  it("fails with a clear error when build_wasm.mjs cannot be spawned", async () => {
    const { existsSync } = await import("node:fs");
    const { spawnSync } = await import("node:child_process");
    const existsSyncMock = vi.mocked(existsSync);
    const spawnSyncMock = vi.mocked(spawnSync);

    existsSyncMock.mockImplementation(() => false);
    spawnSyncMock.mockImplementation(() => {
      return { status: null, error: new Error("ENOENT") } as unknown as ReturnType<typeof spawnSync>;
    });

    const { ensureVariant } = await import("../scripts/ensure_wasm.mjs");
    expect(() => ensureVariant("single")).toThrowError(/Failed to execute build_wasm\.mjs/);
  });
});
