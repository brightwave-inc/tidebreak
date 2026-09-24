//! Per-launch listen endpoint published into the data directory.
//!
//! After a successful bind, the server writes `{data_dir}/listen.json` so a
//! second process can attach without the token riding argv. See
//! [`docs/decisions/0012-data-dir-listen-endpoint.md`] and
//! [`docs/decisions/0022-scoped-local-import-capability.md`]. The
//! client-executor credential is deliberately absent. A separate scoped token
//! permits only publication of bytes the attached process already holds.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};
use tidebreak_core::{AgentError, Result};

/// Filename under the profile data directory.
pub const LISTEN_FILE: &str = "listen.json";

/// What a client needs to reach the process that owns a data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenEndpoint {
    /// Base URL the API is bound on, e.g. `http://127.0.0.1:53421`.
    pub base_url: String,
    /// Per-launch bearer token (full LocalOwner authority).
    pub token: String,
    /// Per-launch capability limited to local document/image publication.
    pub local_import_token: String,
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
                    "no listen endpoint at {} — is a Tidebreak server running \
                     on this data directory? If the desktop app is already \
                     running, set TIDEBREAK_DATA_DIR to its data directory \
                     (macOS: ~/Library/Application Support/io.brightwave.tidebreak, \
                     Linux: ~/.local/share/io.brightwave.tidebreak, \
                     Windows: %APPDATA%\\io.brightwave.tidebreak; debug builds \
                     use io.brightwave.tidebreak.dev) and pass --attach. Or \
                     start the desktop app or `tidebreak serve`, or pass \
                     --server <url> with TIDEBREAK_SERVER_TOKEN",
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
        if endpoint.base_url.trim().is_empty()
            || endpoint.token.trim().is_empty()
            || endpoint.local_import_token.trim().is_empty()
        {
            return Err(AgentError::config(format!(
                "{} is missing base_url, token, or local_import_token",
                path.display()
            )));
        }
        Ok(endpoint)
    }
}

/// Atomically publish the endpoint at `0o600`. Overwrites any prior file.
pub fn write(data_dir: &Path, base_url: &str, token: &str, local_import_token: &str) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .map_err(|error| AgentError::config(format!("failed to create data dir: {error}")))?;
    let endpoint = ListenEndpoint {
        base_url: base_url.trim_end_matches('/').to_owned(),
        token: token.to_owned(),
        local_import_token: local_import_token.to_owned(),
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

/// Remove the file on shutdown when it is still this server's: when it
/// carries the bearer this server minted. A file another process wrote since,
/// with its own bearer, stays.
pub fn remove_if_current(data_dir: &Path, token: &str) {
    if ListenEndpoint::read(data_dir).is_ok_and(|endpoint| endpoint.token == token) {
        remove(data_dir);
    }
}

/// The instance lock file in `data_dir`, whose lock names the process that
/// serves it.
pub fn instance_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(crate::INSTANCE_LOCK_FILE)
}

/// Whether a live process holds `data_dir`'s instance lock, and so whether a
/// `listen.json` there can describe a server that is running.
///
/// The file outlives its server whenever a process ends without its clean
/// shutdown: a crash, a kill, a signal nothing handled. The lock does not,
/// because the kernel releases it with the process. So a reader checks the
/// lock, not the file. This takes a shared lock for as long as it takes to
/// let go of it, and never creates the lock file.
pub fn owner_is_live(data_dir: &Path) -> Result<bool> {
    let path = instance_lock_path(data_dir);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(AgentError::config(format!(
                "could not read {}: {error}",
                path.display()
            )))
        }
    };
    match file.try_lock_shared() {
        Ok(()) => {
            // Nothing holds it, so whatever wrote `listen.json` has exited.
            // Let go at once: a server starting now needs it.
            let _ = file.unlock();
            Ok(false)
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => Err(AgentError::config(format!(
            "could not check {}: {error}",
            path.display()
        ))),
    }
}

/// Holds the published path and removes it when dropped with the server.
pub struct ListenEndpointGuard {
    data_dir: PathBuf,
    token: String,
}

impl ListenEndpointGuard {
    pub fn publish(
        data_dir: PathBuf,
        base_url: &str,
        token: &str,
        local_import_token: &str,
    ) -> Result<Self> {
        write(&data_dir, base_url, token, local_import_token)?;
        Ok(Self {
            data_dir,
            token: token.to_owned(),
        })
    }
}

impl Drop for ListenEndpointGuard {
    fn drop(&mut self) {
        remove_if_current(&self.data_dir, &self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_roundtrip_and_drop_clears() {
        let dir = tempfile::tempdir().unwrap();
        let guard = ListenEndpointGuard::publish(
            dir.path().to_path_buf(),
            "http://127.0.0.1:9/",
            "tok",
            "import-tok",
        )
        .unwrap();
        let read = ListenEndpoint::read(dir.path()).unwrap();
        assert_eq!(read.base_url, "http://127.0.0.1:9");
        assert_eq!(read.token, "tok");
        assert_eq!(read.local_import_token, "import-tok");
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
        assert!(!json.contains("client_executor"));
        drop(guard);
        assert!(ListenEndpoint::read(dir.path()).is_err());
    }

    #[test]
    fn missing_listen_file_names_the_desktop_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let error = ListenEndpoint::read(dir.path()).unwrap_err().to_string();
        assert!(error.contains("is a Tidebreak server running"));
        assert!(!error.contains("is an Tidebreak"));
        assert!(error.contains("TIDEBREAK_DATA_DIR"));
        assert!(error.contains("io.brightwave.tidebreak"));
    }

    /// The lock, not the file, says whether a server is running: a file left
    /// by a server that exited is not believed, and checking never creates a
    /// lock file or keeps a server from taking the lock.
    #[test]
    fn only_a_held_lock_means_a_live_owner() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!owner_is_live(dir.path()).unwrap(), "no lock file");
        assert!(
            !dir.path().join(crate::INSTANCE_LOCK_FILE).exists(),
            "checking must not create the lock file"
        );

        let config = tidebreak_core::Config::desktop(dir.path());
        let lock = crate::InstanceLock::acquire(&config).unwrap();
        assert!(owner_is_live(dir.path()).unwrap(), "a held lock");
        drop(lock);
        assert!(
            !owner_is_live(dir.path()).unwrap(),
            "the file stays after the owner lets go, and means nothing"
        );
        // The check let go of its own shared lock, so a server can start.
        let _lock = crate::InstanceLock::acquire(&config).unwrap();
    }

    /// A server removes the file it published and leaves one another server
    /// wrote since.
    #[test]
    fn shutdown_removes_only_this_servers_file() {
        let dir = tempfile::tempdir().unwrap();
        let guard = ListenEndpointGuard::publish(
            dir.path().to_path_buf(),
            "http://127.0.0.1:9",
            "one",
            "i",
        )
        .unwrap();
        write(dir.path(), "http://127.0.0.1:10", "two", "i").unwrap();
        drop(guard);
        assert_eq!(ListenEndpoint::read(dir.path()).unwrap().token, "two");
        remove_if_current(dir.path(), "two");
        assert!(!ListenEndpoint::path(dir.path()).exists());
    }
}
