use perf::jit::{JitTier, JitTier2Pass};
use perf::telemetry::Telemetry;
use std::hint::black_box;
use std::time::Duration;
use std::time::Instant;

fn burn(iterations: u64) {
    let mut acc = 0u64;
    for i in 0..iterations {
        acc = acc.wrapping_add(i ^ (acc.rotate_left(7)));
    }
    black_box(acc);
}

fn main() {
    let telemetry = Telemetry::new(true);
    telemetry.jit.set_cache_capacity_bytes(256 * 1024 * 1024);

    // Prime rolling window.
    let _ = telemetry.snapshot();

    // Synthetic workload: repeated lookups with occasional compilation.
    let mut used_bytes = 0u64;
    for block in 0..2_000u64 {
        // Simulate a stable working set: most lookups hit, some miss and compile.
        if block % 8 == 0 {
            telemetry.jit.record_cache_miss();

            // Tier 1 baseline compilation.
            telemetry.jit.record_block_compiled(JitTier::Tier1);
            let t0 = Instant::now();
            burn(2_000);
            telemetry.jit.add_compile_time(JitTier::Tier1, t0.elapsed());

            used_bytes += 1_024;
            telemetry.jit.set_cache_used_bytes(used_bytes);

            // Occasionally promote to Tier 2.
            if block % 64 == 0 {
                telemetry.jit.record_block_compiled(JitTier::Tier2);

                let t0 = Instant::now();
                burn(3_000);
                telemetry.jit.add_compile_time(JitTier::Tier2, t0.elapsed());

                // Simulate major passes.
                let pass_start = Instant::now();
                burn(800);
                telemetry
                    .jit
                    .add_tier2_pass_time(JitTier2Pass::ConstFold, pass_start.elapsed());

                let pass_start = Instant::now();
                burn(1_000);
                telemetry
                    .jit
                    .add_tier2_pass_time(JitTier2Pass::Dce, pass_start.elapsed());

                let pass_start = Instant::now();
                burn(1_200);
                telemetry
                    .jit
                    .add_tier2_pass_time(JitTier2Pass::RegAlloc, pass_start.elapsed());
            }
        } else {
            telemetry.jit.record_cache_hit();
        }

        if block == 1_024 {
            telemetry.jit.record_guard_fail();
            telemetry.jit.record_deopt();
        }
    }

    // Ensure the rolling window has a non-zero duration so rates/hit-rate are meaningful.
    std::thread::sleep(Duration::from_millis(10));

    let snapshot = telemetry.snapshot();
    println!("{}", snapshot.jit_hud_line());
    println!("{}", serde_json::to_string_pretty(&snapshot).unwrap());
}
