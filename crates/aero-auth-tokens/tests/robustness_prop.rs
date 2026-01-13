#![cfg(not(target_arch = "wasm32"))]

use aero_auth_tokens::{verify_gateway_session_token, verify_hs256_jwt};
use proptest::prelude::*;

const SECRET: &[u8] = b"prop-test-secret";

fn ascii_segment(max_len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(0u8..=127u8, 0..=max_len)
        .prop_map(|bytes| bytes.into_iter().map(|b| b as char).collect())
}

fn token_strings() -> impl Strategy<Value = String> {
    // Exercise both arbitrary strings and token-shaped inputs with extra dots / long segments.
    prop_oneof![
        10 => ".*",
        2 => proptest::collection::vec(any::<u8>(), 0..=20_000)
            .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        3 => (
            ascii_segment(2_000),
            ascii_segment(2_000),
            ascii_segment(2_000),
            ascii_segment(2_000),
            ascii_segment(2_000),
        )
            .prop_map(|(a, b, c, d, e)| format!("{a}.{b}.{c}.{d}.{e}")),
    ]
}

proptest! {
    // Keep the property tests fast and deterministic enough for CI, while still exploring
    // a wide input space.
    #![proptest_config(ProptestConfig {
        cases: 256,
        .. ProptestConfig::default()
    })]

    #[test]
    fn verify_gateway_session_token_never_panics(token in token_strings()) {
        let now_ms = 1_700_000_000_000u64;
        let res = std::panic::catch_unwind(|| verify_gateway_session_token(&token, SECRET, now_ms));
        prop_assert!(res.is_ok(), "verify_gateway_session_token panicked (len={})", token.len());

        if let Some(verified) = res.unwrap() {
            prop_assert!(!verified.sid.is_empty());
        }
    }

    #[test]
    fn verify_hs256_jwt_never_panics(token in token_strings()) {
        let now_sec = 1_700_000_000i64;
        let res = std::panic::catch_unwind(|| verify_hs256_jwt(&token, SECRET, now_sec));
        prop_assert!(res.is_ok(), "verify_hs256_jwt panicked (len={})", token.len());

        if let Some(claims) = res.unwrap() {
            prop_assert!(!claims.sid.is_empty());
            prop_assert!(claims.exp > now_sec);
        }
    }
}

#[test]
fn extremely_long_inputs_do_not_panic() {
    // Ensure we remain robust against unusually large/malformed token strings without
    // turning this into an expensive test.
    let seg = "x".repeat(200_000);
    let token = format!("{seg}.{seg}.{seg}.{seg}");

    assert!(
        std::panic::catch_unwind(|| verify_gateway_session_token(&token, SECRET, 0)).is_ok(),
        "session token verification panicked on long input"
    );
    assert!(
        std::panic::catch_unwind(|| verify_hs256_jwt(&token, SECRET, 0)).is_ok(),
        "jwt verification panicked on long input"
    );
}

