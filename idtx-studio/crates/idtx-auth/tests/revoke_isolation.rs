//! A 401 from one origin must revoke only that origin's grant, even when the
//! same token value is intentionally shared with an unrelated origin. Regression
//! for the blanket-sweep revocation that broke origin isolation.
use std::time::Duration;

#[test]
fn unauthorized_revokes_only_the_rejected_origin_not_token_siblings() {
    let a = "https://a.example.com";
    let b = "https://b.other.test";
    // Same token value registered independently for two unrelated origins
    // (e.g. one IdP-minted token both services accept).
    idtx_auth::set_bearer_token_for_origins(&[a.into()], "shared").unwrap();
    idtx_auth::set_bearer_token_for_origins(&[b.into()], "shared").unwrap();
    assert_eq!(idtx_auth::authorization_header_for(a).as_deref(), Some("Bearer shared"));
    assert_eq!(idtx_auth::authorization_header_for(b).as_deref(), Some("Bearer shared"));

    // No refresh handler: a 401 for `a` revokes `a` and returns false.
    idtx_auth::set_refresh_handler(None);
    assert!(!idtx_auth::refresh_after_unauthorized(a, Some("Bearer shared"), Duration::from_millis(1)));

    assert_eq!(idtx_auth::authorization_header_for(a), None, "the rejected origin must be revoked");
    assert_eq!(
        idtx_auth::authorization_header_for(b).as_deref(),
        Some("Bearer shared"),
        "a sibling origin sharing the same token value must keep its grant",
    );
}
