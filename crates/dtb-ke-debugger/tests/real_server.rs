//! The debugger's real `SymbolServerSource` against the real `dtb-ke-symbol-server` (in-process, on loopback):
//! upload with the CI token, then resolve with the developer token.

use std::sync::Arc;

use dtb_ke_debugger::identity::identify;
use dtb_ke_debugger::remote::{CacheSource, SymbolServerSource};
use dtb_ke_debugger::{ModuleRef, Outcome, Resolver};
use dtb_ke_symbol_server::config::Config;
use dtb_ke_symbol_server::{AppState, router};

const READ: &str = "read-token-0123456789";
const UPLOAD: &str = "upload-token-0123456789";

async fn start(store: &std::path::Path) -> String {
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        store: store.to_owned(),
        read_token: READ.into(),
        upload_token: UPLOAD.into(),
        max_upload_bytes: 1 << 30,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(AppState::new(config).unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn module_for(exe: &std::path::Path) -> ModuleRef {
    let id = identify(exe).into_iter().next().expect("identity").debug_id;
    ModuleRef {
        base: 0x1000,
        size: 0x1000,
        code_file: "/no/such/place/app".into(),
        debug_file: Some("app".into()),
        debug_id: Some(id),
    }
}

#[tokio::test]
async fn the_debugger_resolves_a_file_from_the_real_server() {
    let exe = std::env::current_exe().unwrap();
    let module = module_for(&exe);
    let id = module.debug_id.unwrap().breakpad().to_string();
    let store = tempfile::tempdir().unwrap();
    let base = start(store.path()).await;

    let put = reqwest::Client::new()
        .put(format!("{base}/v1/debug/{id}?name=app"))
        .bearer_auth(UPLOAD)
        .body(std::fs::read(&exe).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 201);

    let cache_dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(CacheSource::new(cache_dir.path().to_owned()));
    let resolver = Resolver::new()
        .with(SymbolServerSource::new(&base, Some(READ.into()), cache.clone()).unwrap());
    let res = resolver.resolve(&[module.clone()], None, &|_| {}).await;
    let Outcome::Found(found) = &res.modules[0].outcome else {
        panic!("the real server did not resolve the module")
    };
    assert!(found.origin.starts_with("symbol server"), "{}", found.origin);
    assert!(found.path.starts_with(cache_dir.path()));

    // A wrong token is a clear error, not a silent miss.
    let denied = Resolver::new().with(
        SymbolServerSource::new(&base, Some("wrong-token-0123456789".into()), cache).unwrap(),
    );
    let res = denied.resolve(&[module], None, &|_| {}).await;
    let Outcome::Missing { tried } = &res.modules[0].outcome else {
        panic!("resolved with a wrong token")
    };
    assert!(tried[0].contains("401"), "{tried:?}");
}
