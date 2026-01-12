use aero_wasm::guest_cpu_bench::{GuestCpuBenchCoreRunner, ITERS_PER_RUN_CANONICAL, PAYLOADS};

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
        assert!(
            res.retired_instructions > 0,
            "retired instruction count should be non-zero for {}",
            payload.variant.as_str()
        );
    }
}
