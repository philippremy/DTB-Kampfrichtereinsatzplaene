//! The symbol-server client against a stub HTTP server: download → verify → cache, then cache-only.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::identity::{ModuleRef, identify};
use crate::remote::{CacheSource, SymbolServerSource};
use crate::resolve::{Outcome, Resolver};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves `body` for the one `expected` path (any query), 404 for everything else. Counts requests.
async fn stub(
    body: Vec<u8>,
    expected: String,
    hits: Arc<AtomicUsize>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (body, expected, hits) = (body.clone(), expected.clone(), hits.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                hits.fetch_add(1, Ordering::SeqCst);
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let path = path.split('?').next().unwrap_or("");
                let (status, payload) = if path == expected {
                    ("200 OK", body)
                } else {
                    ("404 Not Found", Vec::new())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&payload).await;
            });
        }
    });
    (base, handle)
}

fn module_for(exe: &std::path::Path) -> ModuleRef {
    let id = identify(exe).into_iter().next().expect("identity").debug_id;
    ModuleRef {
        base: 0x1000,
        // Not a path that exists here: the file must come from the server, never the local disk.
        code_file: "/no/such/place/app".into(),
        debug_file: Some("app".into()),
        debug_id: Some(id),
    }
}

#[tokio::test]
async fn downloads_verifies_and_caches() {
    let exe = std::env::current_exe().unwrap();
    let module = module_for(&exe);
    let breakpad = module.debug_id.unwrap().breakpad().to_string();

    let hits = Arc::new(AtomicUsize::new(0));
    let (base, server) = stub(
        std::fs::read(&exe).unwrap(),
        format!("/v1/debug/{breakpad}"),
        hits.clone(),
    )
    .await;
    let cache_dir = std::env::temp_dir().join(format!("dtbke-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache_dir);
    let cache = Arc::new(CacheSource::new(cache_dir.clone()));
    let mut resolver = Resolver::new().with(CacheSource::new(cache_dir.clone()));
    resolver.push(Arc::new(
        SymbolServerSource::new(&base, None, cache).unwrap(),
    ));

    // 1st: cache miss → server → cached.
    let first = resolver.resolve(&[module.clone()], None, &|_| {}).await;
    let Outcome::Found(found) = &first.modules[0].outcome else {
        panic!("not resolved")
    };
    assert!(
        found.origin.starts_with("symbol server"),
        "{}",
        found.origin
    );
    assert!(found.path.starts_with(&cache_dir));
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // 2nd: the server is gone; the cache answers.
    server.abort();
    let second = resolver.resolve(&[module], None, &|_| {}).await;
    let Outcome::Found(found) = &second.modules[0].outcome else {
        panic!("cache miss")
    };
    assert_eq!(found.origin, "cache");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "no second request");
    let _ = std::fs::remove_dir_all(cache_dir);
}

#[tokio::test]
async fn a_file_with_the_wrong_id_is_rejected_and_not_cached() {
    let exe = std::env::current_exe().unwrap();
    let module = module_for(&exe);
    let breakpad = module.debug_id.unwrap().breakpad().to_string();

    // The server answers the right path with the wrong bytes.
    let hits = Arc::new(AtomicUsize::new(0));
    let (base, server) = stub(
        b"not an object file at all".to_vec(),
        format!("/v1/debug/{breakpad}"),
        hits,
    )
    .await;
    let cache_dir = std::env::temp_dir().join(format!("dtbke-cache-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache_dir);
    let cache = Arc::new(CacheSource::new(cache_dir.clone()));
    let resolver = Resolver::new().with(SymbolServerSource::new(&base, None, cache).unwrap());

    let res = resolver.resolve(&[module], None, &|_| {}).await;
    let Outcome::Missing { tried } = &res.modules[0].outcome else {
        panic!("accepted a wrong file")
    };
    assert!(tried[0].contains("does not match"), "{tried:?}");
    assert!(!cache_dir.exists() || std::fs::read_dir(&cache_dir).unwrap().next().is_none());
    server.abort();
    let _ = std::fs::remove_dir_all(cache_dir);
}

#[tokio::test]
async fn unknown_ids_are_a_clean_miss() {
    let exe = std::env::current_exe().unwrap();
    let module = module_for(&exe);
    let (base, server) = stub(
        Vec::new(),
        "/v1/debug/SOMETHING-ELSE".into(),
        Arc::default(),
    )
    .await;
    let cache = Arc::new(CacheSource::new(
        std::env::temp_dir().join("dtbke-cache-none"),
    ));
    let resolver = Resolver::new().with(SymbolServerSource::new(&base, None, cache).unwrap());
    let res = resolver.resolve(&[module], None, &|_| {}).await;
    let Outcome::Missing { tried } = &res.modules[0].outcome else {
        panic!("found from nothing")
    };
    assert!(tried[0].ends_with("not found"), "{tried:?}");
    server.abort();
}
