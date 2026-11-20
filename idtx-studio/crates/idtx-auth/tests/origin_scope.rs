//! One serialized test for the process-global credential boundary.

#[test]
fn credentials_follow_origins_not_hosts_or_paths() {
    idtx_auth::clear();
    let _ = idtx_auth::set_bearer_token_for_hosts(&["https://API.example:443".into()], "scoped");
    assert_eq!(idtx_auth::authorization_header_for("https://api.example/asset.usda").as_deref(), Some("Bearer scoped"));
    for url in ["http://api.example/asset", "https://api.example:8443/asset",
        "https://other.example/asset", "api.example", "https://user:pass@api.example/asset"] {
        assert_eq!(idtx_auth::authorization_header_for(url), None, "unexpected credential for {url}");
    }

    let _ = idtx_auth::set_bearer_token_for_hosts(&["https://*.assets.example:8443".into()], "group");
    for url in ["https://assets.example:8443/root", "https://a.assets.example:8443/x",
        "https://a.b.assets.example:8443/x"] {
        assert_eq!(idtx_auth::authorization_header_for(url).as_deref(), Some("Bearer group"));
    }
    for url in ["https://a.assets.example/x", "http://a.assets.example:8443/x",
        "https://evilassets.example:8443/x"] {
        assert_eq!(idtx_auth::authorization_header_for(url), None);
    }
    idtx_auth::clear();
    assert_eq!(idtx_auth::authorization_header_for("https://api.example/x"), None);
    assert_eq!(idtx_auth::authorization_header_for("https://a.assets.example:8443/x"), None);
}
