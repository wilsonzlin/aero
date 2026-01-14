import { describe, expect, it } from "vitest";

import { runGuestCpuBench } from "./guest_cpu_bench";

describe("bench/guest_cpu_bench option validation", () => {
  it("rejects specifying both seconds and iters", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        seconds: 0.1,
        iters: 1234,
      }),
    ).rejects.toThrow(/mutually exclusive/i);
  });

  it("rejects non-finite seconds", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        seconds: Number.POSITIVE_INFINITY,
      }),
    ).rejects.toThrow(/seconds.*finite/i);
  });

  it("rejects seconds <= 0", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        seconds: 0,
      }),
    ).rejects.toThrow(/seconds.*> 0/i);
  });

  it("validates seconds before rejecting unsupported modes", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "jit_opt",
        seconds: 0,
      }),
    ).rejects.toThrow(/seconds.*> 0/i);
  });

  it("validates iters before rejecting unsupported modes", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "jit_opt",
        iters: 0,
      }),
    ).rejects.toThrow(/iters.*> 0/i);
  });

  it("reports mutual exclusivity even if values are invalid", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "jit_opt",
        seconds: 0,
        iters: 0,
      }),
    ).rejects.toThrow(/mutually exclusive/i);
  });

  it("rejects non-finite iters", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        iters: Number.NaN,
      }),
    ).rejects.toThrow(/iters.*finite/i);
  });

  it("rejects iters <= 0", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        iters: 0,
      }),
    ).rejects.toThrow(/iters.*> 0/i);
  });

  it("rejects fractional iters", async () => {
    await expect(
      runGuestCpuBench({
        variant: "alu32",
        mode: "interpreter",
        iters: 1.5,
      }),
    ).rejects.toThrow(/iters.*integer/i);
  });
});
