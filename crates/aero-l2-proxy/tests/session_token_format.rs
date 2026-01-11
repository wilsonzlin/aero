use aero_l2_proxy::auth::{
    verify_relay_jwt_hs256, verify_session_token, JwtVerifyError, SessionVerifyError,
};

#[test]
fn session_token_rejects_too_many_parts() {
    let err = verify_session_token("a.b.c", b"secret", 0).expect_err("expected invalid token");
    assert_eq!(err, SessionVerifyError::InvalidFormat);
}

#[test]
fn relay_jwt_rejects_empty_segments() {
    // Empty JWT segments should be rejected as malformed before we attempt base64 decode / JSON.
    assert_eq!(
        verify_relay_jwt_hs256(".payload.sig", b"secret", 0).unwrap_err(),
        JwtVerifyError::InvalidFormat
    );
    assert_eq!(
        verify_relay_jwt_hs256("header..sig", b"secret", 0).unwrap_err(),
        JwtVerifyError::InvalidFormat
    );
    assert_eq!(
        verify_relay_jwt_hs256("header.payload.", b"secret", 0).unwrap_err(),
        JwtVerifyError::InvalidFormat
    );
}
