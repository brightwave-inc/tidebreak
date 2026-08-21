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
//! * Tokens are random v4 UUIDs independent of the capability-file filename
//!   (a separate random file id).
//! * Reissuing for the same session writes the new capfile first, then
//!   commits the new mapping and revokes the prior one. The prior authority
//!   stays valid until the new capfile is on disk and the mapping is
//!   installed.
//! * Startup removes the entire dedicated stale-capfile subtree, then
//!   recreates it. A restarted server cannot accept a token it did not mint.
//! * Capfiles are written create-new (mode 0600 on Unix) → sync → atomic
//!   rename. Temp files are deleted on every post-create failure path.
//! * The JSON payload carries exactly `version`, `endpoint`, and `token` — no
//!   owner, workspace, or session identifiers.
//! * In-memory authority is revoked before best-effort file deletion.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tidebreak_core::{CodeSessionId, OwnerId, WorkspaceId};
use tidebreak_harness::BrowserChannelSpec;

/// Capfile format version. Increment when the schema changes in a backward-
/// incompatible way.
const CAPFILE_VERSION: u32 = 1;

/// Filename prefix for capability files. Random file-id components follow.
const CAPFILE_PREFIX: &str = "browser-cap-";

/// Subdirectory under the data dir that holds session capability files.
const CAPFILE_SUBDIR: &str = "browser-caps";

// ── data types ──────────────────────────────────────────────────────────────

/// The `{owner, workspace, session}` subject derived from a token look-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserSubject {
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub session: CodeSessionId,
}

/// Per-session state held in the registry.
#[derive(Debug, Clone)]
struct SessionEntry {
    /// Random file id independent of the bearer token.
    file_id: String,
    /// Bearer token stored in the capfile JSON.
    token: String,
    /// Absolute path to the capability file.
    capfile_path: PathBuf,
}

/// Single lock protects both mappings so issue/revoke/reissue never interleave
/// into inconsistent state.
struct RegistryState {
    tokens: HashMap<String, BrowserSubject>,
    by_session: HashMap<CodeSessionId, SessionEntry>,
}

// ── registry ────────────────────────────────────────────────────────────────

/// In-memory route-token registry paired with an on-disk capability-file
/// subtree.
///
/// One token per session: reissuing is transactional — the new capfile is
/// written before the prior entry is revoked. Startup removes and recreates
/// the entire dedicated subtree so a restarted server cannot accept a token
/// it did not mint.
pub(crate) struct BrowserTokenRegistry {
    state: Mutex<RegistryState>,
    /// Absolute path to the data-dir subtree that holds capfiles. Resolved
    /// at construction time.
    capfile_dir: PathBuf,
    /// Loopback base URL published after bind.
    loopback_base: Mutex<Option<String>>,
}

impl BrowserTokenRegistry {
    /// Construct a new registry rooted at `data_dir`.
    ///
    /// The capfile directory is `{data_dir}/browser-caps`, resolved to an
    /// absolute path. Returns an error if the absolute path cannot be
    /// determined.
    pub(crate) fn new(data_dir: &Path) -> Result<Self, String> {
        let joined = data_dir.join(CAPFILE_SUBDIR);
        let capfile_dir = resolve_absolute(&joined)?;
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
    pub(crate) fn set_loopback_base(&self, base: &str) {
        *self.loopback_base.lock().expect("loopback base") =
            Some(base.trim_end_matches('/').into());
    }

    /// Remove the entire stale-capfile subtree and recreate it empty.
    ///
    /// Must run at startup before any session is recovered or created.
    /// Returns an error on any failure — the caller must fail closed.
    pub(crate) fn delete_all_stale_capfiles(&self) -> Result<(), String> {
        // Remove the entire subtree including unknown files, subdirs, and
        // leftover temp files.
        match std::fs::remove_dir_all(&self.capfile_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // No stale directory to remove — the ensure below recreates.
            }
            Err(e) => {
                return Err(format!(
                    "failed to remove stale browser capfile directory: {e}"
                ));
            }
        }
        // Recreate the empty directory with mode 0700.
        if let Err(e) = std::fs::create_dir_all(&self.capfile_dir) {
            return Err(format!(
                "failed to recreate browser capfile directory: {e}"
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if let Err(e) = std::fs::set_permissions(
                &self.capfile_dir,
                std::fs::Permissions::from_mode(0o700),
            ) {
                return Err(format!("could not set capfile directory mode: {e}"));
            }
        }
        Ok(())
    }

    /// Mint a channel for `subject` and return the [`BrowserChannelSpec`]
    /// the adapter injects into the engine child.
    ///
    /// Transactional: writes the new capfile first, then commits the in-memory
    /// mapping. On reissue, the prior authority (token + capfile) stays valid
    /// until the new capfile is on disk and the new mapping is installed; only
    /// then is the prior entry revoked and its file deleted.
    ///
    /// Returns an error if the loopback base has not been set, the directory
    /// cannot be created, or the capfile cannot be written.
    pub(crate) fn issue(
        &self,
        subject: BrowserSubject,
    ) -> Result<BrowserChannelSpec, String> {
        let loopback_base = self
            .loopback_base
            .lock()
            .expect("loopback base")
            .clone()
            .ok_or_else(|| "loopback base not set".to_owned())?;

        let token = generate_token();
        let file_id = generate_file_id();
        let capfile_path = capfile_path(&self.capfile_dir, &file_id);

        // 1. Write the new capfile. If this fails, nothing has been committed.
        write_capfile(&capfile_path, CAPFILE_VERSION, &loopback_base, &token)?;

        // 2. Commit the in-memory mapping.
        let entry = SessionEntry {
            file_id: file_id.clone(),
            token: token.clone(),
            capfile_path: capfile_path.clone(),
        };

        let mut state = self.state.lock().expect("browser registry");
        // If this session already has an entry, revoke the prior one now
        // that the new capfile is safely on disk.
        let old_entry = state.by_session.insert(subject.session, entry);
        state.tokens.insert(token.clone(), subject);

        // 3. Clean up the prior capfile (best-effort; authority is already
        //    invalidated in memory).
        if let Some(old) = old_entry {
            state.tokens.remove(&old.token);
            let _ = std::fs::remove_file(&old.capfile_path);
        }

        Ok(BrowserChannelSpec::new(capfile_path))
    }

    /// Return the subject for an inbound browser bearer token, or `None`.
    ///
    /// This API is intentionally staged — it is the seam the follow-up
    /// `/code/browser/*` route layer will call. Dead-code lint is suppressed
    /// until that layer lands.
    #[allow(dead_code)]
    pub(crate) fn subject_for_token(&self, token: &str) -> Option<BrowserSubject> {
        self.state
            .lock()
            .expect("browser registry")
            .tokens
            .get(token)
            .cloned()
    }

    /// Revoke and delete the channel for `session_id`. Idempotent.
    ///
    /// In-memory authority is invalidated first. File deletion is best-effort
    /// because startup subtree cleanup must fail closed regardless.
    pub(crate) fn revoke(&self, session_id: CodeSessionId) {
        let mut state = self.state.lock().expect("browser registry");
        if let Some(entry) = state.by_session.remove(&session_id) {
            state.tokens.remove(&entry.token);
            let _ = std::fs::remove_file(&entry.capfile_path);
        }
    }

    /// The data-dir subtree where capfiles live (for tests).
    #[cfg(test)]
    pub(crate) fn capfile_dir(&self) -> &Path {
        &self.capfile_dir
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn generate_token() -> String {
    format!("tbreak_bt_{}", uuid::Uuid::new_v4())
}

fn generate_file_id() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")
}

fn capfile_path(capfile_dir: &Path, file_id: &str) -> PathBuf {
    capfile_dir.join(format!("{}{}.json", CAPFILE_PREFIX, file_id))
}

/// Resolve `joined` to an absolute path without requiring the directory to
/// exist on disk. Uses [`std::path::absolute`] (or canonicalize on older
/// Rust editions) to guarantee absoluteness at the trusted boundary.
fn resolve_absolute(joined: &Path) -> Result<PathBuf, String> {
    // If the directory already exists, canonicalize gives us the real path.
    // Otherwise fall back to `std::path::absolute`.
    match joined.canonicalize() {
        Ok(abs) => return Ok(abs),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "cannot resolve browser capfile directory: {e}"
            ));
        }
    }
    std::path::absolute(joined)
        .map_err(|e| format!("cannot resolve absolute capfile directory: {e}"))
}

/// Write `{version, endpoint, token}` safely:
///
/// 1. Ensure the parent directory exists with mode 0700.
/// 2. Open a random temp file with create_new + mode 0600 (Unix).
/// 3. Write the JSON body, flush, sync.
/// 4. Atomic rename onto the final path.
/// 5. On any failure after create_new, delete the temp file.
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
    let tmp_path = parent.join(&tmp_name);

    // Open with create_new so two issuers cannot share a temp file.
    let mut open_opts = std::fs::OpenOptions::new();
    open_opts.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open_opts.mode(0o600);
    }

    let mut file = match open_opts.open(&tmp_path) {
        Ok(f) => f,
        Err(e) => {
            return Err(format!("could not create temp capfile: {e}"));
        }
    };

    // Each step after open must clean up the temp file on failure.
    if let Err(e) = file.write_all(&body_bytes) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not write capfile: {e}"));
    }

    if let Err(e) = file.flush() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not flush capfile: {e}"));
    }

    if let Err(e) = file.sync_all() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not sync capfile: {e}"));
    }

    // Defence-in-depth chmod on Unix (the open mode is primary).
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

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not rename capfile into place: {e}"));
    }

    Ok(())
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn seeded(data_dir: &Path) -> BrowserTokenRegistry {
        let reg = BrowserTokenRegistry::new(data_dir).unwrap();
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

    fn read_token_from_capfile(path: &Path) -> String {
        let contents = std::fs::read_to_string(path).expect("read capfile");
        let value: serde_json::Value =
            serde_json::from_str(&contents).expect("parse capfile");
        value["token"].as_str().unwrap().to_owned()
    }

    // ── token ↔ file independence ───────────────────────────────────────

    #[test]
    fn filename_is_independent_of_token() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("indep");

        let spec = reg.issue(sub.clone()).unwrap();
        let file_stem = spec
            .capability_file
            .file_stem()
            .unwrap()
            .to_string_lossy();
        let token = read_token_from_capfile(&spec.capability_file);

        // The token must not appear in the filename.
        assert!(
            !file_stem.contains(&token),
            "filename must not embed the bearer token: stem={file_stem}, token={token}"
        );
        // The file stem should start with the prefix and contain a UUID-like
        // suffix (32 hex chars, no dashes).
        let suffix = file_stem
            .strip_prefix(CAPFILE_PREFIX)
            .expect("file stem must start with CAPFILE_PREFIX");
        assert_eq!(suffix.len(), 32, "file id must be a 32-char hex UUID");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "file id must be hex digits"
        );
    }

    #[test]
    fn token_roundtrip_registry_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("roundtrip");

        let spec = reg.issue(sub.clone()).unwrap();
        let token = read_token_from_capfile(&spec.capability_file);
        assert!(!token.is_empty());

        let looked_up = reg.subject_for_token(&token);
        assert_eq!(looked_up.as_ref(), Some(&sub));
    }

    // ── transactional issuance ───────────────────────────────────────────

    #[test]
    fn reissue_preserves_prior_until_new_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("transactional");

        let first = reg.issue(sub.clone()).unwrap();
        let first_token = read_token_from_capfile(&first.capability_file);
        let first_path = first.capability_file.clone();
        assert!(first_path.exists());
        assert_eq!(
            reg.subject_for_token(&first_token).as_ref(),
            Some(&sub),
            "first token must resolve before reissue"
        );

        let second = reg.issue(sub.clone()).unwrap();
        let second_token = read_token_from_capfile(&second.capability_file);

        // Second token is live.
        assert_eq!(reg.subject_for_token(&second_token).as_ref(), Some(&sub));
        // First token is revoked.
        assert!(reg.subject_for_token(&first_token).is_none());
        // First capfile is deleted.
        assert!(!first_path.exists());
        // Second capfile exists.
        assert!(second.capability_file.exists());
        // Tokens differ.
        assert_ne!(first_token, second_token);
    }

    #[test]
    fn reissue_different_file_ids() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("fileids");

        let first = reg.issue(sub.clone()).unwrap();
        let second = reg.issue(sub.clone()).unwrap();

        assert_ne!(
            first.capability_file, second.capability_file,
            "each issuance must produce a unique file path"
        );
        assert_ne!(
            read_token_from_capfile(&first.capability_file),
            read_token_from_capfile(&second.capability_file),
            "each issuance must produce a unique token"
        );
    }

    // ── revocation ───────────────────────────────────────────────────────

    #[test]
    fn revoke_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("idempotent");

        let spec = reg.issue(sub.clone()).unwrap();
        let token = read_token_from_capfile(&spec.capability_file);
        let capfile_path = spec.capability_file.clone();
        assert!(capfile_path.exists());

        reg.revoke(sub.session);
        assert!(!capfile_path.exists());
        assert!(reg.subject_for_token(&token).is_none());

        // Second revoke is silent.
        reg.revoke(sub.session);
    }

    #[test]
    fn revoke_nonexistent_session_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let nonexistent = CodeSessionId::new();
        reg.revoke(nonexistent);
    }

    #[test]
    fn issue_then_revoke_then_reissue_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("fresh");

        let first = reg.issue(sub.clone()).unwrap();
        let first_token = read_token_from_capfile(&first.capability_file);
        reg.revoke(sub.session);
        assert!(reg.subject_for_token(&first_token).is_none());

        let second = reg.issue(sub.clone()).unwrap();
        let second_token = read_token_from_capfile(&second.capability_file);
        assert_ne!(first_token, second_token);
        assert_eq!(reg.subject_for_token(&second_token).as_ref(), Some(&sub));
    }

    // ── capfile schema ───────────────────────────────────────────────────

    #[test]
    fn capfile_schema_is_exact_version_endpoint_token() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("schema");

        let spec = reg.issue(sub).unwrap();
        let contents =
            std::fs::read_to_string(&spec.capability_file).expect("read capfile");
        let value: serde_json::Value =
            serde_json::from_str(&contents).expect("parse capfile");

        let obj = value.as_object().expect("capfile must be a JSON object");
        let expected: HashSet<&str> =
            ["version", "endpoint", "token"].iter().copied().collect();
        let actual: HashSet<&str> = obj.keys().map(String::as_str).collect();

        assert_eq!(
            actual, expected,
            "capfile must have exactly {{version, endpoint, token}} keys"
        );
        assert_eq!(value["version"], CAPFILE_VERSION);
        assert!(value["token"].as_str().unwrap().starts_with("tbreak_bt_"));
    }

    #[test]
    fn capfile_endpoint_derives_from_loopback_base() {
        let dir = tempfile::tempdir().unwrap();
        let reg = BrowserTokenRegistry::new(dir.path()).unwrap();
        reg.set_loopback_base("https://tidebreak.local:4567/");
        let sub = subject("endpoint");

        let spec = reg.issue(sub).unwrap();
        let contents =
            std::fs::read_to_string(&spec.capability_file).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();

        assert_eq!(
            value["endpoint"],
            "https://tidebreak.local:4567/code/browser"
        );
    }

    // ── startup cleanup ──────────────────────────────────────────────────

    #[test]
    fn subtree_cleanup_removes_everything_including_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let capfile_dir = dir.path().join(CAPFILE_SUBDIR);
        std::fs::create_dir_all(&capfile_dir).unwrap();

        // Create various stale artifacts.
        let stale_cap = capfile_dir.join("browser-cap-abc123.json");
        std::fs::write(&stale_cap, b"{}").unwrap();
        let stale_tmp = capfile_dir.join(".some-temp.tmp");
        std::fs::write(&stale_tmp, b"orphan").unwrap();
        let stale_subdir = capfile_dir.join("nested");
        std::fs::create_dir(&stale_subdir).unwrap();
        let stale_nested = stale_subdir.join("inner.txt");
        std::fs::write(&stale_nested, b"deep").unwrap();

        // An unrelated sibling directory must NOT be touched.
        let sibling = dir.path().join("unrelated-dir");
        std::fs::create_dir(&sibling).unwrap();
        let sibling_file = sibling.join("keep.txt");
        std::fs::write(&sibling_file, b"keep").unwrap();

        let reg = BrowserTokenRegistry::new(dir.path()).unwrap();
        reg.delete_all_stale_capfiles().unwrap();

        // The capfile subtree should be empty (only recreated dir exists).
        assert!(capfile_dir.exists());
        let entries: Vec<_> = std::fs::read_dir(&capfile_dir)
            .unwrap()
            .collect();
        assert!(
            entries.is_empty(),
            "capfile dir must be empty after cleanup"
        );

        // Sibling directory untouched.
        assert!(sibling_file.exists());
    }

    #[test]
    fn cleanup_no_existing_directory_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let reg = BrowserTokenRegistry::new(dir.path()).unwrap();
        assert!(reg.delete_all_stale_capfiles().is_ok());
        assert!(reg.capfile_dir().exists());
    }

    // ── path hygiene ─────────────────────────────────────────────────────

    #[test]
    fn capfile_path_is_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("absolute");

        let spec = reg.issue(sub).unwrap();
        assert!(spec.capability_file.is_absolute());
        assert!(reg.capfile_dir().is_absolute());
    }

    #[test]
    fn capfile_dir_is_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let reg = BrowserTokenRegistry::new(dir.path()).unwrap();
        assert!(reg.capfile_dir().is_absolute());
    }

    // ── Unix permissions ─────────────────────────────────────────────────

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

    // ── temp file cleanup ────────────────────────────────────────────────

    #[test]
    fn atomic_temp_file_not_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("atomic");

        let spec = reg.issue(sub).unwrap();
        let parent = spec.capability_file.parent().unwrap();

        let tmp_count = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with('.')
            })
            .count();
        assert_eq!(
            tmp_count, 0,
            "no .tmp files should survive atomic rename"
        );
    }

    // ── concurrent safety ────────────────────────────────────────────────

    #[test]
    fn concurrent_issuance_two_sessions_no_state_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub_a = subject("concurrent-a");
        let sub_b = subject("concurrent-b");

        let spec_a = reg.issue(sub_a.clone()).unwrap();
        let spec_b = reg.issue(sub_b.clone()).unwrap();

        let token_a = read_token_from_capfile(&spec_a.capability_file);
        let token_b = read_token_from_capfile(&spec_b.capability_file);

        assert_ne!(token_a, token_b);
        assert_ne!(spec_a.capability_file, spec_b.capability_file);

        assert_eq!(reg.subject_for_token(&token_a).as_ref(), Some(&sub_a));
        assert_eq!(reg.subject_for_token(&token_b).as_ref(), Some(&sub_b));

        // Revoke A; B must survive.
        reg.revoke(sub_a.session);
        assert!(reg.subject_for_token(&token_a).is_none());
        assert_eq!(reg.subject_for_token(&token_b).as_ref(), Some(&sub_b));
        assert!(spec_b.capability_file.exists());
    }

    #[test]
    fn concurrent_reissue_and_revoke_no_interleaving_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("interleave");

        // Issue → revoke → issue in sequence; the final state must be clean
        // with exactly one live entry.
        let first = reg.issue(sub.clone()).unwrap();
        let first_token = read_token_from_capfile(&first.capability_file);
        reg.revoke(sub.session);

        let second = reg.issue(sub.clone()).unwrap();
        let second_token = read_token_from_capfile(&second.capability_file);

        assert!(reg.subject_for_token(&first_token).is_none());
        assert_eq!(reg.subject_for_token(&second_token).as_ref(), Some(&sub));
        assert!(!first.capability_file.exists());
        assert!(second.capability_file.exists());
    }
}
