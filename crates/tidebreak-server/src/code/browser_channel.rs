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
//! * Reissuing for the same session holds the registry lock across the full
//!   write + commit so revoke cannot interleave and resurrect authority.
//! * Startup synchronously deletes every stale capfile before the returned
//!   future is pollable, so issue cannot race an unpolled cleanup.
//! * Capfiles are written create-new (mode 0600 on Unix) → sync → drop →
//!   atomic rename. Temp files are deleted on every post-create failure path.
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
/// One token per session: reissuing is transactional — the registry lock is
/// held across capfile write + map commit so revoke cannot interleave.
/// Startup synchronously deletes every stale capfile before any issuance
/// can run.
pub(crate) struct BrowserTokenRegistry {
    state: Mutex<RegistryState>,
    /// Absolute path to the data-dir subtree that holds capfiles. Resolved
    /// at construction time and proven free of symlink traversal.
    capfile_dir: PathBuf,
    /// Loopback base URL published after bind.
    loopback_base: Mutex<Option<String>>,
}

impl BrowserTokenRegistry {
    /// Construct a new registry rooted at `data_dir`.
    ///
    /// The capfile directory is `{data_dir}/browser-caps`, resolved to a
    /// lexical absolute path proven not to traverse a symlink outside the
    /// trusted data-dir subtree. Returns an error if resolution fails.
    pub(crate) fn new(data_dir: &Path) -> Result<Self, String> {
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
            return Err(format!("failed to recreate browser capfile directory: {e}"));
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

    /// Mint a channel for `subject` and return the [`BrowserChannelSpec`]
    /// the adapter injects into the engine child.
    ///
    /// Transactional: holds the registry lock across the capfile write and
    /// map commit so `revoke` cannot interleave. On reissue, the prior entry
    /// is held during the write, so its authority remains valid until the
    /// new capfile is on disk; only then is it atomically replaced.
    ///
    /// Returns an error if the loopback base has not been set or the capfile
    /// cannot be written. On error no state is committed.
    pub(crate) fn issue(&self, subject: BrowserSubject) -> Result<BrowserChannelSpec, String> {
        let loopback_base = self
            .loopback_base
            .lock()
            .expect("loopback base")
            .clone()
            .ok_or_else(|| "loopback base not set".to_owned())?;

        let token = generate_token();
        let file_id = generate_file_id();
        let capfile_path = capfile_path(&self.capfile_dir, &file_id);

        // Lock the registry for the full write+commit window. This prevents
        // revoke from deleting a capfile that the write step hasn't created
        // yet, or from resurrecting a session whose old entry we're about to
        // replace. The critical property: while the lock is held, no revoke
        // targeting the same session can observe a state where a capfile
        // exists without a valid registry entry, or vice versa.
        let mut state = self.state.lock().expect("browser registry");
        let old_entry = state.by_session.get(&subject.session).cloned();

        // Write the new capfile. If this fails, unlock and leave state
        // unchanged — nothing was committed.
        match write_capfile(&capfile_path, CAPFILE_VERSION, &loopback_base, &token) {
            Ok(()) => {}
            Err(e) => return Err(e),
        }

        // Commit the new mapping; the new capfile is on disk.
        let entry = SessionEntry {
            token: token.clone(),
            capfile_path: capfile_path.clone(),
        };
        state.by_session.insert(subject.session, entry);
        state.tokens.insert(token.clone(), subject);

        // Revoke the prior authority now that the new one is committed.
        if let Some(old) = old_entry {
            state.tokens.remove(&old.token);
            let _ = std::fs::remove_file(&old.capfile_path);
        }

        Ok(BrowserChannelSpec::new(capfile_path))
    }

    /// Return the subject for an inbound browser bearer token, or `None`.
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
    /// In-memory authority is invalidated first under the registry lock.
    /// File deletion is best-effort because startup subtree cleanup must
    /// fail closed regardless.
    ///
    /// Returns the [`BrowserSubject`] that was mapped to the revoked
    /// session so the caller can derive an adapter scope, or `None` if
    /// the session was not found (idempotent).
    pub(crate) fn revoke(&self, session_id: CodeSessionId) -> Option<BrowserSubject> {
        let mut state = self.state.lock().expect("browser registry");
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

/// Resolve `joined` to a lexical absolute path rooted under the trusted
/// data-dir subtree, proven not to traverse a symlink outside.
///
/// Strategy: canonicalize only the data-dir prefix so any symlink chain in
/// the parent ancestry is resolved normally. Then lexically join the trailing
/// `browser-caps` component and check that the result is NOT an existing
/// symlink — reject it at construction time rather than risk
/// `remove_dir_all` or `create_dir_all` following it.
fn resolve_absolute_trusted(joined: &Path) -> Result<PathBuf, String> {
    // Split into parent (data_dir) and trailing (browser-caps).
    let parent = joined
        .parent()
        .ok_or_else(|| "capfile path has no parent".to_owned())?;
    let suffix = joined
        .file_name()
        .ok_or_else(|| "capfile path has no trailing component".to_owned())?;

    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("cannot resolve data directory for browser capfiles: {e}"))?;

    let absolute = canonical_parent.join(suffix);

    // If the target already exists as a symlink, refuse to construct.
    // This prevents remove_dir_all / create_dir_all from operating on
    // a path that points outside the trusted data-dir subtree.
    match std::fs::symlink_metadata(&absolute) {
        Ok(meta) if meta.is_symlink() => {
            return Err(format!(
                "browser capfile directory is a symlink: {}",
                absolute.display()
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Does not exist yet — fine, it will be created on first use.
        }
        Err(e) => {
            return Err(format!("cannot inspect browser capfile directory: {e}"));
        }
        _ => {
            // Exists and is not a symlink — fine.
        }
    }

    Ok(absolute)
}

/// Write `{version, endpoint, token}` safely:
///
/// 1. Ensure the parent directory exists with mode 0700.
/// 2. Open a random temp file with create_new + mode 0600 (Unix).
/// 3. Write, flush, sync, close (drop).
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
        if let Err(e) = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)) {
            return Err(format!("could not set capfile directory mode: {e}"));
        }
    }

    let endpoint = format!("{loopback_base}/code/browser");

    let body = serde_json::json!({
        "version": version,
        "endpoint": endpoint,
        "token": token,
    });

    let body_bytes =
        serde_json::to_vec(&body).map_err(|e| format!("failed to serialize capfile: {e}"))?;

    // Random temp name to avoid collision with concurrent issuances.
    let tmp_name = format!(".{}.tmp", uuid::Uuid::new_v4().to_string().replace('-', ""));
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
        drop(file);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not write capfile: {e}"));
    }

    if let Err(e) = file.flush() {
        drop(file);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not flush capfile: {e}"));
    }

    if let Err(e) = file.sync_all() {
        drop(file);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not sync capfile: {e}"));
    }

    // Defence-in-depth chmod on Unix (the open mode is primary).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(e) = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
        {
            drop(file);
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!("could not set capfile mode: {e}"));
        }
    }

    // Explicitly drop the file handle before rename. Required for Windows
    // (cannot rename an open handle); no-op on Unix but safe everywhere.
    drop(file);

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
        let value: serde_json::Value = serde_json::from_str(&contents).expect("parse capfile");
        value["token"].as_str().unwrap().to_owned()
    }

    // ── token ↔ file independence ───────────────────────────────────────

    #[test]
    fn filename_is_independent_of_token() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("indep");

        let spec = reg.issue(sub.clone()).unwrap();
        let file_stem = spec.capability_file.file_stem().unwrap().to_string_lossy();
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
    fn reissue_produces_different_file_paths_and_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("fileids");

        let first = reg.issue(sub.clone()).unwrap();
        // Read the token from the first capfile BEFORE second issuance
        // deletes it.
        let first_token = read_token_from_capfile(&first.capability_file);
        let first_path = first.capability_file.clone();

        let second = reg.issue(sub.clone()).unwrap();
        let second_token = read_token_from_capfile(&second.capability_file);

        assert_ne!(first_path, second.capability_file);
        assert_ne!(first_token, second_token);
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
    fn issue_revoke_reissue_produces_fresh_authority() {
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
        let contents = std::fs::read_to_string(&spec.capability_file).expect("read capfile");
        let value: serde_json::Value = serde_json::from_str(&contents).expect("parse capfile");

        let obj = value.as_object().expect("capfile must be a JSON object");
        let expected: HashSet<&str> = ["version", "endpoint", "token"].iter().copied().collect();
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
        let contents = std::fs::read_to_string(&spec.capability_file).unwrap();
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
        let entries: Vec<_> = std::fs::read_dir(&capfile_dir).unwrap().collect();
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

    #[cfg(unix)]
    #[test]
    fn symlinked_browser_caps_entry_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Create a target directory OUTSIDE the data dir.
        let target = dir.path().join("evil-target");
        std::fs::create_dir_all(&target).unwrap();

        // Symlink data/browser-caps → evil-target (outside).
        let link = data_dir.join(CAPFILE_SUBDIR);
        std::os::unix::fs::symlink(&target, &link).unwrap();

        // Construction must FAIL because the browser-caps component is an
        // existing symlink.
        let reg = BrowserTokenRegistry::new(&data_dir);
        assert!(
            reg.is_err(),
            "must reject browser-caps symlink at construction"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_ok_when_trailing_is_not_symlink() {
        let dir = tempfile::tempdir().unwrap();

        // Parent ancestry can have symlinks — canonicalize follows them.
        let real_data = dir.path().join("real-data");
        std::fs::create_dir_all(&real_data).unwrap();
        let link_parent = dir.path().join("data");
        std::os::unix::fs::symlink(&real_data, &link_parent).unwrap();

        // Construction succeeds: the trailing browser-caps is NOT a symlink,
        // and canonicalize(parent) resolves the ancestry normally.
        let reg = BrowserTokenRegistry::new(&link_parent);
        assert!(reg.is_ok());
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
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(tmp_count, 0, "no .tmp files should survive atomic rename");
    }

    #[test]
    fn revoke_and_reissue_produces_clean_slate() {
        // After a successful issuance is revoked (simulating shutdown or
        // error handling), a fresh issuance for the same session must
        // produce a new token, a new capfile path, and no stale in-memory
        // entry. Token is captured before revoke deletes the capfile.
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("cleanslate");

        let first = reg.issue(sub.clone()).unwrap();
        let first_token = read_token_from_capfile(&first.capability_file);
        let first_path = first.capability_file.clone();
        assert!(first_path.exists());

        reg.revoke(sub.session);
        assert!(reg.subject_for_token(&first_token).is_none());
        assert!(!first_path.exists());

        let second = reg.issue(sub.clone()).unwrap();
        let second_token = read_token_from_capfile(&second.capability_file);
        let second_path = second.capability_file.clone();

        assert_ne!(first_token, second_token);
        assert_ne!(first_path, second_path);
        assert_eq!(reg.subject_for_token(&second_token).as_ref(), Some(&sub));
        assert!(second_path.exists());
    }

    #[test]
    fn write_failure_leaves_maps_unchanged() {
        // issue() holds the registry lock across write_capfile + map commit.
        // A write failure drops the lock without mutating either map. Inject a
        // platform-independent failure by temporarily replacing the capfile
        // directory with a regular file, so create_dir_all/write_capfile must
        // fail even when the test process has elevated privileges.
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub = subject("writefail");

        // Warm up: issue once so the capfile directory exists.
        let warm_subject = subject("warm");
        let warm = reg.issue(warm_subject.clone()).unwrap();
        let warm_token = read_token_from_capfile(&warm.capability_file);
        assert!(warm.capability_file.exists());

        let capdir = reg.capfile_dir().to_path_buf();
        let held_capdir = dir.path().join("browser-caps-held");
        std::fs::rename(&capdir, &held_capdir).expect("move capfile directory aside");
        std::fs::write(&capdir, b"not a directory").expect("install capfile path blocker");

        let result = reg.issue(sub.clone());

        // Restore the original directory before asserting so a failure cannot
        // strand the fixture in a state that obscures the registry invariant.
        std::fs::remove_file(&capdir).expect("remove capfile path blocker");
        std::fs::rename(&held_capdir, &capdir).expect("restore capfile directory");
        assert!(
            result.is_err(),
            "issue must fail when capfile parent is a file"
        );
        assert_eq!(
            reg.subject_for_token(&warm_token).as_ref(),
            Some(&warm_subject),
            "the prior mapping must survive the failed issuance"
        );

        // After failure, a fresh issuance for the same session must succeed
        // without stale entries.
        let second = reg.issue(sub.clone()).unwrap();
        let second_token = read_token_from_capfile(&second.capability_file);
        assert_eq!(reg.subject_for_token(&second_token).as_ref(), Some(&sub));

        // The TempDir will clean up everything when dropped.
        let _ = warm;
    }

    // ── sequential safety ────────────────────────────────────────────────

    #[test]
    fn two_sessions_independent_lifecycles() {
        let dir = tempfile::tempdir().unwrap();
        let reg = seeded(dir.path());
        let sub_a = subject("independent-a");
        let sub_b = subject("independent-b");

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
    fn interleaved_reissue_and_revoke_sequence() {
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
