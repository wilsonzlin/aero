declare function initMicrobench(): Promise<void>;

export default initMicrobench;

export function bench_integer_alu(iters: number): unknown;
export function bench_branchy(iters: number): unknown;
export function bench_memcpy(bytes: number, iters: number): unknown;
export function bench_hash(bytes: number, iters: number): unknown;

