//! The HTTP API end to end, through the router (no sockets). The "debug file" is this test executable
//! itself — a real Mach-O / ELF / PE with a real debug id.


use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use dtb_ke_symbol_server::config::Config;
use dtb_ke_symbol_server::{AppState, router};
use tower::ServiceExt as _;

const READ: &str = "read-token-0123456789";
const UPLOAD: &str = "upload-token-0123456789";

fn app(dir: &std::path::Path) -> Router {
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        store: dir.to_owned(),
        read_token: READ.into(),
        upload_token: UPLOAD.into(),
        max_upload_bytes: 512 * 1024 * 1024,
    };
    config.check().unwrap();
    router(AppState::new(config).unwrap())
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-forwarded-for", "203.0.113.7");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
}

/// This executable and the Breakpad id it carries.
fn own_file() -> (Vec<u8>, String) {
    let exe = std::env::current_exe().unwrap();
    let ids = dtb_ke_symid::identify(&exe);
    assert!(
        !ids.is_empty(),
        "the test executable must be identifiable (no build id / UUID?)"
    );
    (std::fs::read(&exe).unwrap(), ids[0].breakpad())
}

#[tokio::test]
async fn health_needs_no_token() {
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = call(&app(dir.path()), Method::GET, "/healthz", None, vec![]).await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"ok".as_slice())
    );
}

#[tokio::test]
async fn upload_then_download_round_trips_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let (bytes, id) = own_file();
    let uri = format!("/v1/debug/{id}?name=tests&commit=abc123&target=x");

    let (status, _) = call(&app, Method::GET, &uri, Some(READ), vec![]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(&app, Method::PUT, &uri, Some(UPLOAD), bytes.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = call(&app, Method::PUT, &uri, Some(UPLOAD), bytes.clone()).await;
    assert_eq!(status, StatusCode::OK, "a repeat upload replaces");

    let (status, body) = call(&app, Method::GET, &uri, Some(READ), vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body == bytes,
        "the downloaded file differs from the uploaded one"
    );
    // The upload token may read too; the id is case-insensitive.
    let lower = format!("/v1/debug/{}", id.to_lowercase());
    assert_eq!(
        call(&app, Method::GET, &lower, Some(UPLOAD), vec![])
            .await
            .0,
        StatusCode::OK
    );

    let meta = std::fs::read_to_string(dir.path().join(&id).join("meta.json")).unwrap();
    assert!(meta.contains("abc123"), "hints are kept: {meta}");
    let leftovers = std::fs::read_dir(dir.path().join(".tmp")).unwrap().count();
    assert_eq!(leftovers, 0, "no scratch files remain");
}

#[tokio::test]
async fn a_file_under_the_wrong_id_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let (bytes, _) = own_file();
    let wrong = "0123456789ABCDEF0123456789ABCDEF1";

    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/debug/{wrong}"),
        Some(UPLOAD),
        bytes,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/debug/{wrong}"),
        Some(UPLOAD),
        b"not an object".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(
        call(
            &app,
            Method::GET,
            &format!("/v1/debug/{wrong}"),
            Some(READ),
            vec![]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join(".tmp")).unwrap().count(),
        0
    );
}

#[tokio::test]
async fn tokens_gate_every_route() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let id = "0123456789ABCDEF0123456789ABCDEF1";
    let uri = format!("/v1/debug/{id}");

    assert_eq!(
        call(&app, Method::GET, &uri, None, vec![]).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, Method::GET, &uri, Some("nope"), vec![]).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, Method::PUT, &uri, Some(READ), vec![]).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, Method::GET, "/v1/debug/not-an-id", Some(READ), vec![])
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            Method::GET,
            "/v1/debug/..%2F..%2Fetc%2Fpasswd",
            Some(READ),
            vec![]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn repeated_bad_tokens_lock_the_client_out() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let uri = "/v1/debug/0123456789ABCDEF0123456789ABCDEF1";
    for _ in 0..10 {
        assert_eq!(
            call(&app, Method::GET, uri, Some("wrong"), vec![]).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    // Even the right token is refused while locked out…
    assert_eq!(
        call(&app, Method::GET, uri, Some(READ), vec![]).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // …but another client is unaffected.
    let other = Request::builder()
        .uri(uri)
        .header("x-forwarded-for", "198.51.100.9")
        .header("authorization", format!("Bearer {READ}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(other).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_size_cap_applies() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        store: dir.path().to_owned(),
        read_token: READ.into(),
        upload_token: UPLOAD.into(),
        max_upload_bytes: 1024,
    };
    let app = router(AppState::new(config).unwrap());
    let (status, _) = call(
        &app,
        Method::PUT,
        "/v1/debug/0123456789ABCDEF0123456789ABCDEF1",
        Some(UPLOAD),
        vec![0; 4096],
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        std::fs::read_dir(dir.path().join(".tmp")).unwrap().count(),
        0
    );
}
