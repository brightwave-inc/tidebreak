//! Per-launch listen endpoint published into the data directory.
//!
//! After a successful bind, the server writes `{data_dir}/listen.json` so a
//! second process can attach without the token riding argv. See
//! [`docs/decisions/0009-data-dir-listen-endpoint.md`]. The client-executor
//! credential is deliberately absent — attach stays bearer-only.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use openwave_core::{AgentError, Result};
use serde::{Deserialize, Serialize};

/// Filename under the profile data directory.
pub const LISTEN_FILE: &str = "listen.json";

/// What a client needs to reach the process that owns a data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenEndpoint {
    /// Base URL the API is bound on, e.g. `http://127.0.0.1:53421`.
    pub base_url: String,
    /// Per-launch bearer token (full LocalOwner authority).
    pub token: String,
}

impl ListenEndpoint {
    /// Path of the listen file under `data_dir`.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(LISTEN_FILE)
    }

    /// Read a previously published endpoint, or explain why attach cannot.
    pub fn read(data_dir: &Path) -> Result<Self> {
        let path = Self::path(data_dir);
        let bytes = std::fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AgentError::config(format!(
                    "no listen endpoint at {} — is an OpenWave server running \
                     on this data directory? Start the desktop app or \
                     `openwave serve`, or pass --server <url> with \
                     OPENWAVE_SERVER_TOKEN",
                    path.display()
                ))
            } else {
                AgentError::config(format!("failed to read {}: {error}", path.display()))
            }
        })?;
        let endpoint: Self = serde_json::from_slice(&bytes).map_err(|error| {
            AgentError::config(format!(
                "{} is not a valid listen endpoint ({error}); remove it or \
                 restart the server that owns this data directory",
                path.display()
            ))
        })?;
        if endpoint.base_url.trim().is_empty() || endpoint.token.trim().is_empty() {
            return Err(AgentError::config(format!(
                "{} is missing base_url or token",
                path.display()
            )));
        }
        Ok(endpoint)
    }
}

/// Atomically publish the endpoint at `0o600`. Overwrites any prior file.
pub fn write(data_dir: &Path, base_url: &str, token: &str) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .map_err(|error| AgentError::config(format!("failed to create data dir: {error}")))?;
    let endpoint = ListenEndpoint {
        base_url: base_url.trim_end_matches('/').to_owned(),
        token: token.to_owned(),
    };
    let mut bytes = serde_json::to_vec_pretty(&endpoint).map_err(|error| {
        AgentError::config(format!("failed to encode listen endpoint: {error}"))
    })?;
    bytes.push(b'\n');
    let path = ListenEndpoint::path(data_dir);
    let temporary = data_dir.join(format!(".{LISTEN_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    let mut published = false;
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &path)?;
        published = true;
        Ok(())
    })();
    if result.is_err() && !published {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| {
        AgentError::config(format!(
            "failed to publish listen endpoint {}: {error}",
            path.display()
        ))
    })
}

/// Best-effort removal on clean shutdown.
pub fn remove(data_dir: &Path) {
    let _ = std::fs::remove_file(ListenEndpoint::path(data_dir));
}

/// Holds the published path and removes it when dropped with the server.
pub struct ListenEndpointGuard {
    data_dir: PathBuf,
}

impl ListenEndpointGuard {
    pub fn publish(data_dir: PathBuf, base_url: &str, token: &str) -> Result<Self> {
        write(&data_dir, base_url, token)?;
        Ok(Self { data_dir })
    }
}

impl Drop for ListenEndpointGuard {
    fn drop(&mut self) {
        remove(&self.data_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_roundtrip_and_drop_clears() {
        let dir = tempfile::tempdir().unwrap();
        let guard =
            ListenEndpointGuard::publish(dir.path().to_path_buf(), "http://127.0.0.1:9/", "tok")
                .unwrap();
        let read = ListenEndpoint::read(dir.path()).unwrap();
        assert_eq!(read.base_url, "http://127.0.0.1:9");
        assert_eq!(read.token, "tok");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(ListenEndpoint::path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let json = std::fs::read_to_string(ListenEndpoint::path(dir.path())).unwrap();
        assert!(!json.contains("executor"));
        drop(guard);
        assert!(ListenEndpoint::read(dir.path()).is_err());
    }
}
