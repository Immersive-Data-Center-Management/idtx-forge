use std::{sync::{Arc, mpsc}, time::Duration};

#[test]
fn refresh_is_bounded_scoped_and_cannot_resurrect_logout() {
    let origin = "https://refresh.test";
    idtx_auth::set_bearer_token_for_origins(&[origin.into()], "old").unwrap();
    idtx_auth::set_refresh_handler(Some(Arc::new(|url| {
        assert_eq!(url, "https://refresh.test"); Some("new".into())
    })));
    assert!(idtx_auth::refresh_after_unauthorized(origin, Some("Bearer old"), Duration::from_secs(1)));
    assert_eq!(idtx_auth::authorization_header_for(origin).as_deref(), Some("Bearer new"));
    assert_eq!(idtx_auth::authorization_header_for("http://refresh.test"), None);
    let (tx, rx) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let wait = std::sync::Mutex::new(wait);
    idtx_auth::set_refresh_handler(Some(Arc::new(move |_| {
        tx.send(()).unwrap(); wait.lock().unwrap().recv().unwrap(); Some("late".into())
    })));
    let pending = std::thread::spawn(move || idtx_auth::refresh_after_unauthorized(origin, Some("Bearer new"), Duration::from_secs(1)));
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    idtx_auth::clear();
    release.send(()).unwrap();
    assert!(!pending.join().unwrap());
    assert_eq!(idtx_auth::authorization_header_for(origin), None);
    assert!(!idtx_auth::refresh_after_unauthorized(origin, Some("Bearer new"), Duration::from_millis(1)));
    idtx_auth::set_refresh_handler(None);
    idtx_auth::set_bearer_token_for_origins(&[origin.into()], "timeout-old").unwrap();
    let (release, wait) = mpsc::channel();
    let wait = std::sync::Mutex::new(wait);
    idtx_auth::set_refresh_handler(Some(Arc::new(move |_| { wait.lock().unwrap().recv().unwrap(); Some("too-late".into()) })));
    let started = std::time::Instant::now();
    assert!(!idtx_auth::refresh_after_unauthorized(origin, Some("Bearer timeout-old"), Duration::from_millis(10)));
    assert!(started.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    assert_eq!(idtx_auth::authorization_header_for(origin), None);
    idtx_auth::set_refresh_handler(None);
    idtx_auth::set_bearer_token_for_origins(&["https://other.test".into()], "other").unwrap();
    idtx_auth::AuthScopes::parse(&[origin.into()]).unwrap().clear().unwrap();
    assert_eq!(idtx_auth::authorization_header_for("https://other.test").as_deref(), Some("Bearer other"));
}
