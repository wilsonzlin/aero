export default function initMicrobench(): Promise<void>;

export function bench_integer_alu(iters: number): number | bigint;
export function bench_branchy(iters: number): number | bigint;
export function bench_memcpy(bytes: number, iters: number): number | bigint;
export function bench_hash(bytes: number, iters: number): number | bigint;
