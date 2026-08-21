//! Session-browser authority: mint, write, and revoke browser capability
//! files for engine sessions.
//!
//! Each code session that needs browser access receives a scoped bearer token
//! mapped through an in-memory route-token registry. The token plus the
//! loopback endpoint are written into a session-private JSON capability file
//! whose path is injected into the engine child. No owner, workspace, or
//! session identifiers leave the server.
//!
//! ## Security properties
//!
//! * Tokens are random v4 UUIDs; reissuing for the same session silently
//!   revokes and deletes the prior token and its capability file.
//! * Startup deletes every stale capability file so a restarted server cannot
//!   accept a token it did not mint.
//! * Capfiles are written create-new → sync → chmod 0600 → atomic rename.
//! * The JSON payload carries only `version`, `endpoint`, and `token` — no
//!   owner, workspace, or session identifiers.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tidebreak_core::{CodeSessionId, OwnerId, WorkspaceId};
use tidebreak_harness::BrowserChannelSpec;

/// Capfile format version. Increment when the schema changes in a backward-
/// incompatible way.
const CAPFILE_VERSION: u32 = 1;

/// Filename prefix for capability files. Random components follow.
const CAPFILE_PREFIX: &str = "browser-cap-";

/// Subdirectory under the data dir that holds session capability files.
const CAPFILE_SUBDIR: &str = "browser-caps";

/// The `{owner, workspace, session}` subject derived from a token look-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserSubject {
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub session: CodeSessionId,
}

/// In-memory route-token registry paired with an on-disk capability-file tree.
///
/// One token per session: reissuing silently revokes and deletes the prior
/// token and its capability file. Startup deletes every stale file so a
/// restarted server cannot accept a token it did not mint.
pub(crate) struct BrowserTokenRegistry {
    tokens: Mutex<HashMap<String, BrowserSubject>>,
    by_session: Mutex<HashMap<CodeSessionId, String>>,
    /// Absolute path to the data-dir subtree that holds capfiles.
    capfile_dir: PathBuf,
    /// Loopback base URL published after bind.
    loopback_base: Mutex<Option<String>>,
}

impl BrowserTokenRegistry {
    pub(crate) fn new(data_dir: &Path) -> Self {
        let capfile_dir = data_dir.join(CAPFILE_SUBDIR);
        Self {
            tokens: Mutex::new(HashMap::new()),
            by_session: Mutex::new(HashMap::new()),
            capfile_dir,
            loopback_base: Mutex::new(None),
        }
    }

    /// Publish the bound loopback base so later [`Self::issue`] calls can
    /// write it into the capfile endpoint.
    pub(crate) fn set_loopback_base(&self, base: &str) {
        *self.loopback_base.lock().expect("loopback base") =
            Some(base.trim_end_matches('/').into());
    }

    /// Delete every stale capability file under the capfile directory.
    ///
    /// Must run at startup before any session is recovered or created so a
    /// restarted server cannot accept tokens it did not mint. The in-memory
    /// registry is always fresh on restart.
    pub(crate) fn delete_all_stale_capfiles(&self) {
        match std::fs::read_dir(&self.capfile_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|f| f.starts_with(CAPFILE_PREFIX))
                    {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // The directory does not exist yet — nothing to clean.
            }
            Err(e) => {
                tracing::warn!(
                    "code-mode: could not read browser capfile directory for cleanup: {e}"
                );
            }
        }
    }

    /// Mint a channel for `subject` and return the [`BrowserChannelSpec`]
    /// the adapter injects into the engine child.
    ///
    /// The returned path is always absolute because it is resolved against the
    /// absolute `data_dir` (closing PR #2364's accepted P3 about constructor-
    /// level path validation — the server is the single trusted constructor
    /// site and guarantees absoluteness at this boundary).
    ///
    /// Reissuing for the same session revokes and deletes the prior token
    /// and its capability file.
    pub(crate) fn issue(&self, subject: BrowserSubject) -> Result<BrowserChannelSpec, String> {
        let token = generate_token();
        let loopback_base = self
            .loopback_base
            .lock()
            .expect("loopback base")
            .clone()
            .ok_or_else(|| "loopback base not set".to_owned())?;

        // Revoke and delete any prior token for this session.
        {
            let mut by_session = self.by_session.lock().expect("browser session tokens");
            if let Some(old) = by_session.insert(subject.session, token.clone()) {
                let mut tokens = self.tokens.lock().expect("browser tokens");
                tokens.remove(&old);
                let old_path = capfile_path(&self.capfile_dir, &old);
                let _ = std::fs::remove_file(&old_path);
            }
            let mut tokens = self.tokens.lock().expect("browser tokens");
            tokens.insert(token.clone(), subject);
        }

        let capfile_path = capfile_path(&self.capfile_dir, &token);
        write_capfile(&capfile_path, CAPFILE_VERSION, &loopback_base, &token)?;

        Ok(BrowserChannelSpec::new(capfile_path))
    }

    /// Return the subject for an inbound browser bearer token, or `None`.
    pub(crate) fn subject_for_token(&self, token: &str) -> Option<BrowserSubject> {
        self.tokens
            .lock()
            .expect("browser tokens")
            .get(token)
            .cloned()
    }

    /// Revoke and delete the channel for `session_id`. Idempotent.
    pub(crate) fn revoke(&self, session_id: CodeSessionId) {
        let token = self
            .by_session
            .lock()
            .expect("browser session tokens")
            .remove(&session_id);
        if let Some(token) = token {
            self.tokens.lock().expect("browser tokens").remove(&token);
            let path = capfile_path(&self.capfile_dir, &token);
            let _ = std::fs::remove_file(&path);
        }
    }

    /// The data-dir subtree where capfiles live (for tests).
    #[cfg(test)]
    pub(crate) fn capfile_dir(&self) -> &Path {
        &self.capfile_dir
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn generate_token() -> String {
    format!("tbreak_bt_{}", uuid::Uuid::new_v4())
}

fn capfile_path(capfile_dir: &Path, token: &str) -> PathBuf {
    capfile_dir.join(format!("{}{}.json", CAPFILE_PREFIX, token))
}

/// Write `{version, endpoint, token}` safely:
///
/// 1. Ensure the directory exists with mode 0700.
/// 2. Write to a random temp name under the capfile dir.
/// 3. Write the JSON body.
/// 4. fsync the file.
/// 5. Set mode 0600.
/// 6. Atomic rename onto the final path.
fn write_capfile(
    path: &Path,
    version: u32,
    loopback_base: &str,
    token: &str,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "capfile has no parent directory".to_owned())?;

    if let Err(e) = std::fs::create_dir_all(parent) {
        return Err(format!("could not create capfile directory: {e}"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(e) =
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
        {
            return Err(format!("could not set capfile directory mode: {e}"));
        }
    }

    let endpoint = format!("{loopback_base}/code/browser");

    let body = serde_json::json!({
        "version": version,
        "endpoint": endpoint,
        "token": token,
    });

    let body_bytes = serde_json::to_vec(&body)
        .map_err(|e| format!("failed to serialize capfile: {e}"))?;

    // Random temp name to avoid collision with concurrent issuances.
    let tmp_name = format!(
        ".{}.tmp",
        uuid::Uuid::new_v4().to_string().replace('-', "")
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("could not create temp capfile: {e}"))?;

        file.write_all(&body_bytes)
            .map_err(|e| format!("could not write capfile: {e}"))?;

        file.flush()
            .map_err(|e| format!("could not flush capfile: {e}"))?;

        file.sync_all()
            .map_err(|e| format!("could not sync capfile: {e}"))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if let Err(e) =
                std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
            {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(format!("could not set capfile mode: {e}"));
            }
        }
    }

    std::fs::rename(&tmp_path, path)
        .map_err(|e| format!("could not rename capfile into place: {e}"))?;

    Ok(())
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(data_dir: &Path) -> BrowserTokenRegistry {
        let reg = BrowserTokenRegistry::new(data_dir);
        reg.set_loopback_base("http://127.0.0.1:0");
        reg
    }

    fn subject(_label: &str) -> BrowserSubject {
        BrowserSubject {
            owner: OwnerId::local(),
            workspace: WorkspaceId::new(),
            session: CodeSessionId::new(),
        }
    }

    #[test]
    fn token_roundtrip_registry_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("roundtrip");

        let spec = reg.issue(sub.clone()).unwrap();
        let token = extract_token_from_path(&spec.capability_file);
        assert!(!token.is_empty());

        let looked_up = reg.subject_for_token(&token);
        assert_eq!(looked_up.as_ref(), Some(&sub));
    }

    #[test]
    fn reissue_revokes_and_replaces_prior_token() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("reissue");

        let first = reg.issue(sub.clone()).unwrap();
        let first_token = extract_token_from_path(&first.capability_file);
        assert!(first.capability_file.exists());

        let second = reg.issue(sub.clone()).unwrap();
        let second_token = extract_token_from_path(&second.capability_file);

        assert_ne!(first_token, second_token);
        assert!(reg.subject_for_token(&first_token).is_none());
        assert_eq!(reg.subject_for_token(&second_token).as_ref(), Some(&sub));
        assert!(!first.capability_file.exists());
        assert!(second.capability_file.exists());
    }

    #[test]
    fn revoke_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("idempotent");

        let spec = reg.issue(sub.clone()).unwrap();
        let token = extract_token_from_path(&spec.capability_file);
        assert!(spec.capability_file.exists());

        reg.revoke(sub.session);
        assert!(!spec.capability_file.exists());
        assert!(reg.subject_for_token(&token).is_none());

        // Second revoke is silent.
        reg.revoke(sub.session);
    }

    #[test]
    fn capfile_schema_has_no_subject_ids() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("schema");

        let spec = reg.issue(sub).unwrap();
        let contents = std::fs::read_to_string(&spec.capability_file).expect("read capfile");
        let value: serde_json::Value = serde_json::from_str(&contents).expect("parse capfile");

        assert!(value.get("version").is_some());
        assert!(value.get("endpoint").is_some());
        assert!(value.get("token").is_some());
        assert!(value.get("owner").is_none());
        assert!(value.get("workspace").is_none());
        assert!(value.get("session").is_none());
        assert!(value.get("browser_id").is_none());
    }

    #[test]
    fn capfile_endpoint_derives_from_loopback_base() {
        let dir = tempfile::tempdir().unwrap();
        let reg = BrowserTokenRegistry::new(dir.path());
        reg.set_loopback_base("https://tidebreak.local:4567/");
        let sub = subject("endpoint");

        let spec = reg.issue(sub).unwrap();
        let contents = std::fs::read_to_string(&spec.capability_file).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();

        assert_eq!(
            value["endpoint"],
            "https://tidebreak.local:4567/code/browser"
        );
    }

    #[test]
    fn stale_directory_cleanup_removes_known_capfiles() {
        let dir = tempfile::tempdir().unwrap();
        let capfile_dir = dir.path().join(CAPFILE_SUBDIR);
        std::fs::create_dir_all(&capfile_dir).unwrap();

        let stale_path = capfile_dir.join(format!("{}{}.json", CAPFILE_PREFIX, uuid::Uuid::new_v4()));
        std::fs::write(&stale_path, b"{}").unwrap();
        assert!(stale_path.exists());

        // A non-capfile in the same dir must survive.
        let other = capfile_dir.join("not-a-browser-file.txt");
        std::fs::write(&other, b"do not delete").unwrap();

        let reg = BrowserTokenRegistry::new(dir.path());
        reg.delete_all_stale_capfiles();

        assert!(!stale_path.exists());
        assert!(other.exists());
    }

    #[test]
    fn cleanup_handles_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let reg = BrowserTokenRegistry::new(dir.path());
        reg.delete_all_stale_capfiles();
    }

    #[test]
    fn capfile_path_is_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("absolute");

        let spec = reg.issue(sub).unwrap();
        assert!(spec.capability_file.is_absolute());
    }

    #[cfg(unix)]
    #[test]
    fn capfile_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("mode");

        let spec = reg.issue(sub).unwrap();
        let meta = std::fs::metadata(&spec.capability_file).unwrap();
        let mode = meta.permissions().mode();

        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn capfile_directory_is_mode_0700() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("dirmode");

        let _spec = reg.issue(sub).unwrap();
        let capfile_dir = reg.capfile_dir();
        let meta = std::fs::metadata(capfile_dir).unwrap();
        let mode = meta.permissions().mode();

        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn atomic_temp_file_is_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("atomic");

        let spec = reg.issue(sub).unwrap();
        let parent = spec.capability_file.parent().unwrap();

        let tmp_count = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(tmp_count, 0, "no .tmp files should survive atomic rename");
    }

    fn extract_token_from_path(path: &Path) -> String {
        let name = path.file_stem().unwrap().to_string_lossy();
        let without_prefix = name.strip_prefix(CAPFILE_PREFIX).unwrap();
        without_prefix.to_owned()
    }
}
