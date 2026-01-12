use aero_wasm::guest_cpu_bench::{
    GuestCpuBenchCoreRunner, GuestCpuBenchVariant, ITERS_PER_RUN_CANONICAL, PAYLOADS,
};

fn expected_retired_instructions_10k(variant: GuestCpuBenchVariant) -> u64 {
    let iters = u64::from(ITERS_PER_RUN_CANONICAL);
    match variant {
        GuestCpuBenchVariant::Alu64 | GuestCpuBenchVariant::Alu32 => 3 + iters * 7,
        GuestCpuBenchVariant::BranchPred64 | GuestCpuBenchVariant::BranchPred32 => 3 + iters * 10,
        GuestCpuBenchVariant::BranchUnpred64 | GuestCpuBenchVariant::BranchUnpred32 => 4 + iters * 16,
        GuestCpuBenchVariant::MemSeq64
        | GuestCpuBenchVariant::MemSeq32
        | GuestCpuBenchVariant::MemStride64
        | GuestCpuBenchVariant::MemStride32 => 3 + iters * 8,
        GuestCpuBenchVariant::CallRet64 | GuestCpuBenchVariant::CallRet32 => 3 + iters * 11,
    }
}

#[test]
fn guest_cpu_payload_checksums_match_pf008_doc() {
    let mut runner = GuestCpuBenchCoreRunner::new();

    for payload in PAYLOADS {
        let res = runner
            .run_payload_once(payload, ITERS_PER_RUN_CANONICAL)
            .unwrap_or_else(|e| panic!("{} failed: {e}", payload.variant.as_str()));
        assert_eq!(
            res.checksum,
            payload.expected_checksum_10k,
            "checksum mismatch for {}",
            payload.variant.as_str()
        );
        assert_eq!(
            res.retired_instructions,
            expected_retired_instructions_10k(payload.variant),
            "retired instruction count mismatch for {}",
            payload.variant.as_str()
        );
        assert!(
            res.retired_instructions > 0,
            "retired instruction count should be non-zero for {}",
            payload.variant.as_str()
        );
    }
}

#[test]
fn guest_cpu_payload_rejects_zero_iters() {
    let mut runner = GuestCpuBenchCoreRunner::new();
    let payload = &PAYLOADS[0];
    let err = runner.run_payload_once(payload, 0).unwrap_err();
    assert!(
        matches!(err, aero_wasm::guest_cpu_bench::GuestCpuBenchError::InvalidIters(0)),
        "unexpected error: {err}"
    );
}
