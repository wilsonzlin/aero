#[test]
fn no_lingering_nstk_references_in_docs_and_tests() {
    // Older snapshots used an accidental 4CC for the user-space network stack snapshot blob.
    // The canonical 4CC is `NETS`, and we intentionally avoid mentioning the legacy spelling in
    // docs/tests to prevent confusion.
    const LEGACY_4CC: [u8; 4] = [0x4e, 0x53, 0x54, 0x4b];

    let files: [(&str, &str); 4] = [
        (
            "docs/16-snapshots.md",
            include_str!("../../../docs/16-snapshots.md"),
        ),
        (
            "crates/aero-io-snapshot/src/io/network/state.rs",
            include_str!("../../aero-io-snapshot/src/io/network/state.rs"),
        ),
        (
            "crates/aero-net-stack/tests/snapshot_roundtrip.rs",
            include_str!("../../aero-net-stack/tests/snapshot_roundtrip.rs"),
        ),
        (
            "crates/aero-wasm/tests/vm_snapshot_builder_roundtrip.rs",
            include_str!("../../aero-wasm/tests/vm_snapshot_builder_roundtrip.rs"),
        ),
    ];

    for (path, contents) in files {
        assert!(
            !contents.as_bytes().windows(LEGACY_4CC.len()).any(|w| w == LEGACY_4CC),
            "lingering legacy 4CC reference found in {path}"
        );
    }
}
