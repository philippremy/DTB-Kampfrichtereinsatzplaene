//! Settings, read once from the environment (the systemd unit's `EnvironmentFile`).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, bail};

const MIN_TOKEN_LEN: usize = 16;

#[derive(Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub store: PathBuf,
    /// May `GET` (developers' debuggers).
    pub read_token: String,
    /// May `GET` and `PUT` (CI).
    pub upload_token: String,
    pub max_upload_bytes: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let required = |name: &str| var(name).with_context(|| format!("{name} is not set"));

        let listen = var("DTB_KE_SYMBOLS_LISTEN").unwrap_or_else(|| "127.0.0.1:8080".into());
        let max_mb: u64 = match var("DTB_KE_SYMBOLS_MAX_UPLOAD_MB") {
            Some(v) => v
                .parse()
                .context("DTB_KE_SYMBOLS_MAX_UPLOAD_MB is not a number")?,
            None => 1024,
        };
        let config = Self {
            listen: listen
                .parse()
                .with_context(|| format!("DTB_KE_SYMBOLS_LISTEN: `{listen}` is not an address"))?,
            store: required("DTB_KE_SYMBOLS_STORE")?.into(),
            read_token: required("DTB_KE_SYMBOLS_READ_TOKEN")?,
            upload_token: required("DTB_KE_SYMBOLS_UPLOAD_TOKEN")?,
            max_upload_bytes: max_mb * 1024 * 1024,
        };
        config.check()?;
        Ok(config)
    }

    pub fn check(&self) -> anyhow::Result<()> {
        for (name, token) in [("read", &self.read_token), ("upload", &self.upload_token)] {
            if token.len() < MIN_TOKEN_LEN {
                bail!("the {name} token is shorter than {MIN_TOKEN_LEN} characters");
            }
        }
        if self.read_token == self.upload_token {
            bail!("the read and upload tokens must differ");
        }
        Ok(())
    }
}
