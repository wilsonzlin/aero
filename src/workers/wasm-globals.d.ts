export {};

declare global {
  /**
   * Global shims used by the wasm32 browser runtime.
   *
   * Note: WebAssembly `i64` values are represented as JS `bigint` in the JS WebAssembly API.
   * Any function that returns/accepts a wasm `i64` must use `bigint` (not `number`).
   */
  interface WindowOrWorkerGlobalScope {
    /**
     * Tier-1 JIT dispatch hook used by `crates/aero-wasm`'s tiered VM.
     *
     * Wasm signature: `__aero_jit_call(table_index: i32, cpu_ptr: i32, jit_ctx_ptr: i32) -> i64`.
     */
    __aero_jit_call?: (tableIndex: number, cpuPtr: number, jitCtxPtr: number) => bigint;

    /**
     * Port I/O shims for the minimal VM loop (`crates/aero-wasm/src/vm.rs`).
     */
    __aero_io_port_read?: (port: number, size: number) => number;
    __aero_io_port_write?: (port: number, size: number, value: number) => void;

    /**
     * MMIO shims used by the minimal VM loop and/or device models.
     *
     * Note: `addr` is wasm `u64`, represented as JS `bigint`.
     */
    __aero_mmio_read?: (addr: bigint, size: number) => number;
    __aero_mmio_write?: (addr: bigint, size: number, value: number) => void;
  }
}

