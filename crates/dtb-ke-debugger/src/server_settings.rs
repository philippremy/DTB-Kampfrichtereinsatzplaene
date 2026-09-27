//! The symbol server's address and token, remembered between runs.
//!
//! The debugger's data root is a per-run temporary directory (`main`), so these live in the OS **config**
//! directory instead: `<config>/de.philippremy.DTB-KE-Debugger/symbol-server.toml`. The token is stored as plain
//! text in that file (owner-only permissions on Unix) — it is a read token for a debug-file archive, not a secret
//! that protects user data, and the OS keychains would tie the developer tool to three different APIs.
//!
//! `DTB_KE_SYMBOL_SERVER` / `DTB_KE_SYMBOL_TOKEN` still work, as the default when nothing is stored.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::remote::ServerConfig;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    pub url: String,
    pub token: String,
}

const FILE: &str = "symbol-server.toml";

/// `<OS config dir>/de.philippremy.DTB-KE-Debugger`.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("de.philippremy.DTB-KE-Debugger")
}

/// What is stored in `dir`; defaults for a missing or unreadable file (which is logged, never fatal).
pub fn load(dir: &Path) -> Stored {
    let path = dir.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
            log::warn!("ignoring {}: {err}", path.display());
            Stored::default()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Stored::default(),
        Err(err) => {
            log::warn!("cannot read {}: {err}", path.display());
            Stored::default()
        }
    }
}

/// Writes `stored` into `dir` (atomically; owner-only on Unix). An empty address removes the file.
pub fn save(dir: &Path, stored: &Stored) -> std::io::Result<()> {
    let path = dir.join(FILE);
    if stored.url.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(dir)?;
    let text = toml::to_string_pretty(stored).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("toml.part");
    {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(&tmp)?.write_all(text.as_bytes())?;
    }
    std::fs::rename(&tmp, &path)
}

/// The server to use at startup: the stored one, else the environment's.
pub fn initial(stored: &Stored) -> Stored {
    if !stored.url.is_empty() {
        return stored.clone();
    }
    Stored {
        url: std::env::var("DTB_KE_SYMBOL_SERVER").unwrap_or_default(),
        token: std::env::var("DTB_KE_SYMBOL_TOKEN").unwrap_or_default(),
    }
}

/// The usable config for a stored value (`None` when no address is set or it is malformed).
pub fn to_config(stored: &Stored) -> Option<ServerConfig> {
    ServerConfig::parse(&stored.url, &stored.token)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_server_round_trips_and_an_empty_one_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Stored::default());

        let stored = Stored {
            url: "https://symbols.example.net".into(),
            token: "t0ken".into(),
        };
        save(dir.path(), &stored).unwrap();
        assert_eq!(load(dir.path()), stored);
        assert_eq!(
            to_config(&stored),
            Some(ServerConfig {
                base: "https://symbols.example.net".into(),
                token: Some("t0ken".into())
            })
        );
        assert!(!dir.path().join("symbol-server.toml.part").exists());

        save(dir.path(), &Stored::default()).unwrap();
        assert!(!dir.path().join(FILE).exists());
        assert_eq!(load(dir.path()), Stored::default());
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        save(
            dir.path(),
            &Stored {
                url: "https://x.example".into(),
                token: "secret".into(),
            },
        )
        .unwrap();
        let mode = std::fs::metadata(dir.path().join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn an_unreadable_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "url = [not toml").unwrap();
        assert_eq!(load(dir.path()), Stored::default());
    }

    #[test]
    fn typed_addresses_are_normalised_and_validated() {
        assert_eq!(ServerConfig::parse("  ", "x").unwrap(), None);
        assert_eq!(
            ServerConfig::parse(" https://s.example/ ", " tok ").unwrap(),
            Some(ServerConfig {
                base: "https://s.example".into(),
                token: Some("tok".into())
            })
        );
        assert_eq!(
            ServerConfig::parse("http://10.0.0.5:8080", "")
                .unwrap()
                .unwrap()
                .token,
            None
        );
        assert!(ServerConfig::parse("symbols.example.net", "").is_err());
    }
}
