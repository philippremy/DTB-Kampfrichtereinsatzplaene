//! Bearer-token checks with a per-client lockout for repeated failures.
//!
//! The server sits behind a reverse proxy that sets `X-Forwarded-For`, so the client key comes from that
//! header (there is no other peer to tell apart).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode, header};
use subtle::ConstantTimeEq;

use crate::config::Config;

const MAX_FAILURES: u32 = 10;
const WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_TRACKED: usize = 10_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Read,
    Upload,
}

#[derive(Default)]
pub struct Limiter {
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

impl Limiter {
    fn blocked(&self, client: &str) -> bool {
        let map = self.failures.lock().unwrap();
        matches!(map.get(client), Some((n, since)) if *n >= MAX_FAILURES && since.elapsed() < WINDOW)
    }

    fn record_failure(&self, client: &str) {
        let mut map = self.failures.lock().unwrap();
        if map.len() > MAX_TRACKED {
            map.retain(|_, (_, since)| since.elapsed() < WINDOW);
        }
        let entry = map.entry(client.to_owned()).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    fn clear(&self, client: &str) {
        self.failures.lock().unwrap().remove(client);
    }
}

pub fn client_key(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// `Ok(())` if the request may do `role`; otherwise the status to answer with.
pub fn authorize(
    config: &Config,
    limiter: &Limiter,
    headers: &HeaderMap,
    role: Role,
) -> Result<(), StatusCode> {
    let client = client_key(headers);
    if limiter.blocked(&client) {
        log::warn!("auth: {client} is locked out after repeated failures");
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    let is_upload = eq(presented, &config.upload_token);
    let is_read = eq(presented, &config.read_token);
    match (is_upload, is_read, role) {
        (true, _, _) | (_, true, Role::Read) => {
            limiter.clear(&client);
            Ok(())
        }
        (_, true, Role::Upload) => Err(StatusCode::FORBIDDEN),
        _ => {
            limiter.record_failure(&client);
            log::warn!("auth: bad or missing token from {client}");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}
