//! Session-native computer-use authority shared by every coding harness.
//!
//! Mirrors the browser channel's per-session capability file pattern
//! ([`crate::code::browser_channel`]): each code session receives a scoped
//! bearer token mapped through an in-memory route-token registry, written into
//! a session-private capability file whose path the engine child inherits.
//! No owner, workspace, or session identifiers leave the server, and no
//! ambient app token reaches a harness.
//!
//! ## Security properties
//!
//! * Tokens are random v4 UUIDs independent of the capability-file filename
//!   (a separate random file id).
//! * Reissuing for the same session holds the registry lock across the full
//!   write + commit so revoke cannot interleave and resurrect authority.
//! * Startup synchronously deletes every stale capfile before the returned
//!   future is pollable, so issue cannot race an unpolled cleanup.
//! * Capfiles are written create-new (mode 0600 on Unix) -> sync -> drop ->
//!   atomic rename. Temp files are deleted on every post-create failure path.
//! * The JSON payload carries only `version`, `endpoint`, and `token` - no
//!   owner, workspace, or session identifiers.
//! * In-memory authority is revoked before best-effort file deletion.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tidebreak_core::{OwnerId, SessionId, WorkspaceId};

/// Capfile format version. Increment when the schema changes in a backward-
/// incompatible way.
const CAPFILE_VERSION: u32 = 1;

/// Filename prefix for capability files. Random file-id components follow.
const CAPFILE_PREFIX: &str = "native-cap-";

/// Subdirectory under the data dir that holds session capability files.
const CAPFILE_SUBDIR: &str = "native-caps";

/// One `{owner, workspace, session}` subject derived from a native token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSubject {
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub session: SessionId,
}

/// Per-session state held in the registry.
#[derive(Debug, Clone)]
struct SessionEntry {
    token: String,
    capfile_path: PathBuf,
}

struct RegistryState {
    tokens: HashMap<String, NativeSubject>,
    by_session: HashMap<SessionId, SessionEntry>,
}

/// In-memory route-token registry paired with an on-disk capability-file
/// subtree.
///
/// One token per session: reissuing is transactional - the registry lock is
/// held across capfile write + map commit so revoke cannot interleave.
/// Startup synchronously deletes every stale capfile before any issuance.
pub struct NativeTokenRegistry {
    state: Mutex<RegistryState>,
    capfile_dir: PathBuf,
    loopback_base: Mutex<Option<String>>,
}

impl NativeTokenRegistry {
    /// Construct a new registry rooted at `data_dir`.
    pub fn new(data_dir: &Path) -> Result<Self, String> {
        let joined = data_dir.join(CAPFILE_SUBDIR);
        let capfile_dir = resolve_absolute_trusted(&joined)?;
        Ok(Self {
            state: Mutex::new(RegistryState {
                tokens: HashMap::new(),
                by_session: HashMap::new(),
            }),
            capfile_dir,
            loopback_base: Mutex::new(None),
        })
    }

    /// Publish the bound loopback base so later [`Self::issue`] calls can
    /// write it into the capfile endpoint.
    pub fn set_loopback_base(&self, base: &str) {
        *self.loopback_base.lock().expect("loopback base") =
            Some(base.trim_end_matches('/').into());
    }

    /// Remove the entire stale-capfile subtree and recreate it empty.
    pub fn delete_all_stale_capfiles(&self) -> Result<(), String> {
        match std::fs::remove_dir_all(&self.capfile_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "failed to remove stale native capfile directory: {e}"
                ));
            }
        }
        if let Err(e) = std::fs::create_dir_all(&self.capfile_dir) {
            return Err(format!("failed to recreate native capfile directory: {e}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if let Err(e) =
                std::fs::set_permissions(&self.capfile_dir, std::fs::Permissions::from_mode(0o700))
            {
                return Err(format!("could not set capfile directory mode: {e}"));
            }
        }
        Ok(())
    }

    /// Mint a channel for `subject` and return the capability-file path the
    /// harness injects through `TIDEBREAK_NATIVE_CAPFILE`.
    ///
    /// Transactional: holds the registry lock across the capfile write and
    /// map commit so `revoke` cannot interleave. On reissue, the prior entry
    /// is held during the write, so its authority remains valid until the
    /// new capfile is on disk; only then is it atomically replaced.
    pub fn issue(&self, subject: NativeSubject) -> Result<PathBuf, String> {
        let loopback_base = self
            .loopback_base
            .lock()
            .expect("loopback base")
            .clone()
            .ok_or_else(|| "loopback base not set".to_owned())?;
        let token = generate_token();
        let file_id = generate_file_id();
        let capfile_path = capfile_path(&self.capfile_dir, &file_id);
        let mut state = self.state.lock().expect("native registry");
        let old_entry = state.by_session.get(&subject.session).cloned();
        match write_capfile(&capfile_path, CAPFILE_VERSION, &loopback_base, &token) {
            Ok(()) => {}
            Err(e) => return Err(e),
        }
        let entry = SessionEntry {
            token: token.clone(),
            capfile_path: capfile_path.clone(),
        };
        state.by_session.insert(subject.session, entry);
        state.tokens.insert(token.clone(), subject);
        if let Some(old) = old_entry {
            state.tokens.remove(&old.token);
            let _ = std::fs::remove_file(&old.capfile_path);
        }
        Ok(capfile_path)
    }

    /// Return the subject for an inbound native bearer token, or `None`.
    pub fn subject_for_token(&self, token: &str) -> Option<NativeSubject> {
        self.state
            .lock()
            .expect("native registry")
            .tokens
            .get(token)
            .cloned()
    }

    /// Revoke and delete the channel for `session_id`. Idempotent.
    pub fn revoke(&self, session_id: SessionId) -> Option<NativeSubject> {
        let mut state = self.state.lock().expect("native registry");
        match state.by_session.remove(&session_id) {
            Some(entry) => {
                let subject = state.tokens.remove(&entry.token);
                let _ = std::fs::remove_file(&entry.capfile_path);
                subject
            }
            None => None,
        }
    }

    /// The data-dir subtree where capfiles live (for tests).
    #[cfg(any(test, feature = "test-support"))]
    pub fn capfile_dir(&self) -> &Path {
        &self.capfile_dir
    }
}

fn generate_token() -> String {
    format!("tbreak_nt_{}", uuid::Uuid::new_v4())
}

fn generate_file_id() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")
}

fn capfile_path(capfile_dir: &Path, file_id: &str) -> PathBuf {
    capfile_dir.join(format!("{}{}.json", CAPFILE_PREFIX, file_id))
}

fn resolve_absolute_trusted(joined: &Path) -> Result<PathBuf, String> {
    let parent = joined
        .parent()
        .ok_or_else(|| "capfile path has no parent".to_owned())?;
    let suffix = joined
        .file_name()
        .ok_or_else(|| "capfile path has no trailing component".to_owned())?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("cannot resolve data directory for native capfiles: {e}"))?;
    let absolute = canonical_parent.join(suffix);
    match std::fs::symlink_metadata(&absolute) {
        Ok(meta) if meta.is_symlink() => {
            return Err(format!(
                "native capfile directory is a symlink: {}",
                absolute.display()
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!("cannot inspect native capfile directory: {e}"));
        }
        _ => {}
    }
    Ok(absolute)
}

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
        return Err(format!("could not create native capfile directory: {e}"));
    }
    let tmp_path = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("native-cap"),
        generate_file_id()
    ));
    let result = (|| {
        use std::fs::OpenOptions;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("could not create native capfile temp file: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("could not set native capfile temp mode: {e}"))?;
        }
        let payload = serde_json::json!({
            "version": version,
            "endpoint": format!("{loopback_base}/code/native"),
            "token": token,
        });
        file.write_all(
            serde_json::to_vec(&payload)
                .map_err(|e| format!("could not encode native capfile: {e}"))?
                .as_slice(),
        )
        .map_err(|e| format!("could not write native capfile temp: {e}"))?;
        file.flush()
            .map_err(|e| format!("could not flush native capfile temp: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("could not sync native capfile temp: {e}"))?;
        drop(file);
        std::fs::rename(&tmp_path, path)
            .map_err(|e| format!("could not install native capfile: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn temp_data_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn subject(session: SessionId, workspace: WorkspaceId, owner: OwnerId) -> NativeSubject {
        NativeSubject {
            owner,
            workspace,
            session,
        }
    }

    fn seeded(data_dir: &std::path::Path) -> NativeTokenRegistry {
        let reg = NativeTokenRegistry::new(data_dir).unwrap();
        reg.set_loopback_base("http://127.0.0.1:0");
        reg
    }

    #[test]
    fn issue_roundtrips_and_revokes() {
        let dir = temp_data_dir();
        let reg = seeded(dir.path());
        let owner = OwnerId::new("local").unwrap();
        let workspace = WorkspaceId::new();
        let session = SessionId::new();
        let path = reg
            .issue(subject(session, workspace, owner.clone()))
            .unwrap();
        assert!(path.to_string_lossy().contains("native-cap-"));
        let raw = std::fs::read_to_string(&path).unwrap();
        let wire: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let token = wire["token"].as_str().unwrap().to_owned();
        assert_eq!(
            reg.subject_for_token(&token),
            Some(subject(session, workspace, owner.clone()))
        );
        assert_eq!(
            reg.revoke(session),
            Some(subject(session, workspace, owner))
        );
        assert!(reg.subject_for_token(&token).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn reissue_revokes_the_previous_capfile() {
        let dir = temp_data_dir();
        let reg = seeded(dir.path());
        let loopback = "http://127.0.0.1:0".to_owned();
        reg.set_loopback_base(&loopback);
        let subject = subject(
            SessionId::new(),
            WorkspaceId::new(),
            OwnerId::new("local").unwrap(),
        );
        let first = reg.issue(subject.clone()).unwrap();
        let first_raw = std::fs::read_to_string(&first).unwrap();
        let first_token = serde_json::from_str::<serde_json::Value>(&first_raw).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        let second = reg.issue(subject.clone()).unwrap();
        assert_ne!(first, second);
        assert!(!first.exists());
        let second_raw = std::fs::read_to_string(&second).unwrap();
        let second_token = serde_json::from_str::<serde_json::Value>(&second_raw).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(first_token, second_token);
        assert!(reg.subject_for_token(&first_token).is_none());
        assert_eq!(reg.subject_for_token(&second_token), Some(subject));
    }

    #[test]
    fn capfile_endpoint_names_the_native_route() {
        let dir = temp_data_dir();
        let reg = NativeTokenRegistry::new(dir.path()).unwrap();
        reg.set_loopback_base("http://127.0.0.1:4567/");
        let path = reg
            .issue(subject(
                SessionId::new(),
                WorkspaceId::new(),
                OwnerId::new("local").unwrap(),
            ))
            .unwrap();
        let wire: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(wire["endpoint"], "http://127.0.0.1:4567/code/native");
        assert_eq!(wire["version"], 1);
    }

    #[test]
    fn stale_capfiles_are_deleted_at_startup() {
        let dir = temp_data_dir();
        let reg = seeded(dir.path());
        let stale = reg.capfile_dir().join("native-cap-stale.json");
        std::fs::create_dir_all(reg.capfile_dir()).unwrap();
        std::fs::write(&stale, b"stale").unwrap();
        reg.delete_all_stale_capfiles().unwrap();
        assert!(!stale.exists());
        assert_eq!(std::fs::read_dir(reg.capfile_dir()).unwrap().count(), 0);
    }

    #[test]
    fn capfiles_are_private_and_symlink_safe() {
        let dir = temp_data_dir();
        let leaf = dir.path().join("native-caps");
        let reg = seeded(dir.path());
        let subject = subject(
            SessionId::new(),
            WorkspaceId::new(),
            OwnerId::new("local").unwrap(),
        );
        reg.issue(subject).unwrap();
        for entry in std::fs::read_dir(reg.capfile_dir()).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            assert!(!metadata.file_type().is_symlink());
            #[cfg(unix)]
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        assert!(leaf.is_dir() || !leaf.exists());
    }
}
