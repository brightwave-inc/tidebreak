//! Durable, native-owned browser restart metadata.
//!
//! The store records only completed navigation state. Live engine handles,
//! document epochs, controller capabilities, grants, and in-flight work stay
//! in memory so a restart cannot replay authority or an unfinished action.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tidebreak_core::{replace_file, sync_directory, OwnerId};
use url::Url;
use uuid::Uuid;

const FILE_NAME: &str = "browser-sessions.json";
const VERSION: u8 = 2;
const PREVIOUS_FILE_VERSION: u8 = 1;
const LEGACY_RENDERER_VERSION: u8 = 1;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SESSIONS: usize = 256;
const MAX_LEGACY_IMPORT_ACKNOWLEDGEMENTS: usize = 1024;
const MAX_BROWSER_ID_CHARS: usize = 80;
const MAX_WORKSPACE_ID_CHARS: usize = 200;
const MAX_URL_CHARS: usize = 8_192;
const MAX_TITLE_CHARS: usize = 160;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveredBrowserSession {
    pub(crate) url: String,
    pub(crate) title: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LegacyBrowserSession {
    pub(crate) version: u8,
    pub(crate) browser_id: String,
    pub(crate) workspace_id: String,
    pub(crate) url: Option<String>,
    pub(crate) title: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LegacyBrowserImportStatus {
    Imported,
    NativeStateKept,
    AlreadyHandled,
    Discarded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LegacyBrowserImportResult {
    pub(crate) status: LegacyBrowserImportStatus,
    pub(crate) browser_id: String,
    pub(crate) workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct BrowserSessionStore {
    inner: Arc<Mutex<BrowserSessionState>>,
}

#[derive(Default)]
struct BrowserSessionState {
    directory: Option<PathBuf>,
    path: Option<PathBuf>,
    sessions: HashMap<String, StoredBrowserSession>,
    legacy_import_acknowledgements:
        HashMap<LegacyBrowserImportKey, StoredLegacyBrowserImportAcknowledgement>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBrowserSessionsV1 {
    version: u8,
    sessions: Vec<StoredBrowserSession>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBrowserSessionsV2 {
    version: u8,
    sessions: Vec<StoredBrowserSession>,
    legacy_import_acknowledgements: Vec<StoredLegacyBrowserImportAcknowledgement>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBrowserSession {
    browser_id: String,
    owner_id: OwnerId,
    workspace_id: String,
    committed_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LegacyBrowserImportKey {
    owner_id: OwnerId,
    browser_id: String,
    workspace_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredLegacyBrowserImportAcknowledgement {
    owner_id: OwnerId,
    browser_id: String,
    workspace_id: String,
    acknowledged_at: DateTime<Utc>,
}

#[derive(Default)]
struct LoadedBrowserSessionState {
    sessions: Vec<StoredBrowserSession>,
    legacy_import_acknowledgements: Vec<StoredLegacyBrowserImportAcknowledgement>,
}

impl BrowserSessionStore {
    pub(crate) fn initialize(&self, data_dir: &Path) -> Result<(), String> {
        fs::create_dir_all(data_dir)
            .map_err(|_| "could not create private browser state".to_owned())?;
        let path = data_dir.join(FILE_NAME);
        let loaded = load_state(&path)?;
        let mut state = self.lock();
        if let Some(existing) = &state.path {
            return if existing == &path {
                Ok(())
            } else {
                Err("browser session storage was initialized more than once".to_owned())
            };
        }
        state.directory = Some(data_dir.to_path_buf());
        state.path = Some(path);
        state.sessions = loaded
            .sessions
            .into_iter()
            .map(|session| (session.browser_id.clone(), session))
            .collect();
        state.legacy_import_acknowledgements = loaded
            .legacy_import_acknowledgements
            .into_iter()
            .map(|acknowledgement| (acknowledgement_key(&acknowledgement), acknowledgement))
            .collect();
        Ok(())
    }

    pub(crate) fn recover(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<Option<RecoveredBrowserSession>, String> {
        validate_identity(browser_id, workspace_id)?;
        let state = self.lock();
        let Some(session) = state.sessions.get(browser_id) else {
            return Ok(None);
        };
        ensure_binding(session, owner_id, workspace_id)?;
        Ok(Some(RecoveredBrowserSession {
            url: session.committed_url.clone(),
            title: session.title.clone(),
        }))
    }

    pub(crate) fn ensure_binding(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        validate_identity(browser_id, workspace_id)?;
        if let Some(session) = self.lock().sessions.get(browser_id) {
            ensure_binding(session, owner_id, workspace_id)?;
        }
        Ok(())
    }

    pub(crate) fn commit(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
        url: &str,
        title: Option<&str>,
    ) -> Result<(), String> {
        validate_identity(browser_id, workspace_id)?;
        let committed_url = validated_url(url)?;
        let title = clean_title(title);
        let mut state = self.lock();
        if let Some(existing) = state.sessions.get(browser_id) {
            ensure_binding(existing, owner_id, workspace_id)?;
        } else if state.sessions.len() >= MAX_SESSIONS {
            return Err("browser session storage is full".to_owned());
        }
        let mut next = state.sessions.clone();
        next.insert(
            browser_id.to_owned(),
            StoredBrowserSession {
                browser_id: browser_id.to_owned(),
                owner_id: owner_id.clone(),
                workspace_id: workspace_id.to_owned(),
                committed_url,
                title,
                updated_at: Utc::now(),
            },
        );
        persist_if_initialized(&state, &next, &state.legacy_import_acknowledgements)?;
        state.sessions = next;
        Ok(())
    }

    pub(crate) fn update_title(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
        url: &str,
        title: Option<&str>,
    ) -> Result<bool, String> {
        validate_identity(browser_id, workspace_id)?;
        let committed_url = validated_url(url)?;
        let title = clean_title(title);
        let mut state = self.lock();
        let Some(existing) = state.sessions.get(browser_id) else {
            return Ok(false);
        };
        ensure_binding(existing, owner_id, workspace_id)?;
        if existing.committed_url != committed_url {
            return Ok(false);
        }
        if existing.title == title {
            return Ok(true);
        }
        let mut next = state.sessions.clone();
        let session = next
            .get_mut(browser_id)
            .expect("browser session was checked above");
        session.title = title;
        session.updated_at = Utc::now();
        persist_if_initialized(&state, &next, &state.legacy_import_acknowledgements)?;
        state.sessions = next;
        Ok(true)
    }

    pub(crate) fn import_legacy(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
        legacy: Option<LegacyBrowserSession>,
    ) -> Result<LegacyBrowserImportResult, String> {
        validate_identity(browser_id, workspace_id)?;
        let key = LegacyBrowserImportKey {
            owner_id: owner_id.clone(),
            browser_id: browser_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
        };
        let mut state = self.lock();
        if state.legacy_import_acknowledgements.contains_key(&key) {
            return Ok(legacy_import_result(
                LegacyBrowserImportStatus::AlreadyHandled,
                browser_id,
                workspace_id,
                session_for_binding(&state.sessions, owner_id, browser_id, workspace_id),
            ));
        }
        if state.legacy_import_acknowledgements.len() >= MAX_LEGACY_IMPORT_ACKNOWLEDGEMENTS {
            return Err("browser legacy import storage is full".to_owned());
        }

        let existing = state.sessions.get(browser_id).cloned();
        let (status, imported, result_session) = match existing {
            Some(session)
                if session.owner_id == *owner_id && session.workspace_id == workspace_id =>
            {
                (
                    LegacyBrowserImportStatus::NativeStateKept,
                    None,
                    Some(session),
                )
            }
            Some(_) => (LegacyBrowserImportStatus::Discarded, None, None),
            None => match legacy.and_then(|legacy| {
                imported_legacy_session(owner_id, browser_id, workspace_id, legacy)
            }) {
                Some(session) => (
                    LegacyBrowserImportStatus::Imported,
                    Some(session.clone()),
                    Some(session),
                ),
                None => (LegacyBrowserImportStatus::Discarded, None, None),
            },
        };

        if imported.is_some() && state.sessions.len() >= MAX_SESSIONS {
            return Err("browser session storage is full".to_owned());
        }
        let mut next_sessions = state.sessions.clone();
        if let Some(session) = imported {
            next_sessions.insert(browser_id.to_owned(), session);
        }
        let mut next_acknowledgements = state.legacy_import_acknowledgements.clone();
        next_acknowledgements.insert(
            key,
            StoredLegacyBrowserImportAcknowledgement {
                owner_id: owner_id.clone(),
                browser_id: browser_id.to_owned(),
                workspace_id: workspace_id.to_owned(),
                acknowledged_at: Utc::now(),
            },
        );
        persist_if_initialized(&state, &next_sessions, &next_acknowledgements)?;
        state.sessions = next_sessions;
        state.legacy_import_acknowledgements = next_acknowledgements;
        Ok(legacy_import_result(
            status,
            browser_id,
            workspace_id,
            result_session.as_ref(),
        ))
    }

    pub(crate) fn forget(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        validate_identity(browser_id, workspace_id)?;
        let mut state = self.lock();
        if let Some(existing) = state.sessions.get(browser_id) {
            ensure_binding(existing, owner_id, workspace_id)?;
        }
        let key = LegacyBrowserImportKey {
            owner_id: owner_id.clone(),
            browser_id: browser_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
        };
        if !state.legacy_import_acknowledgements.contains_key(&key)
            && state.legacy_import_acknowledgements.len() >= MAX_LEGACY_IMPORT_ACKNOWLEDGEMENTS
        {
            return Err("browser legacy import storage is full".to_owned());
        }
        let mut next = state.sessions.clone();
        next.remove(browser_id);
        let mut next_acknowledgements = state.legacy_import_acknowledgements.clone();
        next_acknowledgements.entry(key).or_insert_with(|| {
            StoredLegacyBrowserImportAcknowledgement {
                owner_id: owner_id.clone(),
                browser_id: browser_id.to_owned(),
                workspace_id: workspace_id.to_owned(),
                acknowledged_at: Utc::now(),
            }
        });
        persist_if_initialized(&state, &next, &next_acknowledgements)?;
        state.sessions = next;
        state.legacy_import_acknowledgements = next_acknowledgements;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, BrowserSessionState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn ensure_binding(
    session: &StoredBrowserSession,
    owner_id: &OwnerId,
    workspace_id: &str,
) -> Result<(), String> {
    if session.owner_id != *owner_id || session.workspace_id != workspace_id {
        Err("browser session belongs to a different workspace".to_owned())
    } else {
        Ok(())
    }
}

fn session_for_binding<'a>(
    sessions: &'a HashMap<String, StoredBrowserSession>,
    owner_id: &OwnerId,
    browser_id: &str,
    workspace_id: &str,
) -> Option<&'a StoredBrowserSession> {
    sessions
        .get(browser_id)
        .filter(|session| session.owner_id == *owner_id && session.workspace_id == workspace_id)
}

fn legacy_import_result(
    status: LegacyBrowserImportStatus,
    browser_id: &str,
    workspace_id: &str,
    session: Option<&StoredBrowserSession>,
) -> LegacyBrowserImportResult {
    LegacyBrowserImportResult {
        status,
        browser_id: browser_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        url: session.map(|session| session.committed_url.clone()),
        title: session.and_then(|session| session.title.clone()),
    }
}

fn imported_legacy_session(
    owner_id: &OwnerId,
    browser_id: &str,
    workspace_id: &str,
    legacy: LegacyBrowserSession,
) -> Option<StoredBrowserSession> {
    if legacy.version != LEGACY_RENDERER_VERSION
        || legacy.browser_id != browser_id
        || legacy.workspace_id != workspace_id
    {
        return None;
    }
    let committed_url = validated_url(legacy.url.as_deref()?).ok()?;
    Some(StoredBrowserSession {
        browser_id: browser_id.to_owned(),
        owner_id: owner_id.clone(),
        workspace_id: workspace_id.to_owned(),
        committed_url,
        title: clean_title(legacy.title.as_deref()),
        updated_at: Utc::now(),
    })
}

fn validate_identity(browser_id: &str, workspace_id: &str) -> Result<(), String> {
    if browser_id.is_empty()
        || browser_id.chars().count() > MAX_BROWSER_ID_CHARS
        || !browser_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("browser session id is not valid".to_owned());
    }
    if workspace_id.is_empty()
        || workspace_id.chars().count() > MAX_WORKSPACE_ID_CHARS
        || workspace_id.chars().any(char::is_control)
    {
        return Err("browser workspace id is not valid".to_owned());
    }
    Ok(())
}

fn validated_url(value: &str) -> Result<String, String> {
    if value.chars().count() > MAX_URL_CHARS || value.chars().any(char::is_control) {
        return Err("browser session URL is not valid".to_owned());
    }
    let url = Url::parse(value).map_err(|_| "browser session URL is not valid".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("browser session URL is not valid".to_owned());
    }
    Ok(url.to_string())
}

fn clean_title(value: Option<&str>) -> Option<String> {
    let value = value?;
    if value
        .chars()
        .any(|character| character.is_control() && !character.is_whitespace())
    {
        return None;
    }
    let mut title = String::new();
    let mut title_chars = 0;
    for word in value.split_whitespace() {
        if !title.is_empty() && title_chars < MAX_TITLE_CHARS {
            title.push(' ');
            title_chars += 1;
        }
        for character in word.chars() {
            if title_chars >= MAX_TITLE_CHARS {
                return Some(title);
            }
            title.push(character);
            title_chars += 1;
        }
    }
    (!title.is_empty()).then_some(title)
}

fn load_state(path: &Path) -> Result<LoadedBrowserSessionState, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedBrowserSessionState::default())
        }
        Err(_) => return Err("browser session storage is unavailable".to_owned()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("browser session storage is invalid".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("browser session storage has broad permissions".to_owned());
        }
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Ok(LoadedBrowserSessionState::default());
    }
    let bytes = fs::read(path).map_err(|_| "browser session storage is unavailable".to_owned())?;
    let Ok(header) = serde_json::from_slice::<StoredBrowserSessionsHeader>(&bytes) else {
        return Ok(LoadedBrowserSessionState::default());
    };
    match header.version {
        PREVIOUS_FILE_VERSION => {
            let Ok(stored) = serde_json::from_slice::<StoredBrowserSessionsV1>(&bytes) else {
                return Ok(LoadedBrowserSessionState::default());
            };
            if validate_sessions(PREVIOUS_FILE_VERSION, &stored.sessions).is_err() {
                return Ok(LoadedBrowserSessionState::default());
            }
            Ok(LoadedBrowserSessionState {
                sessions: stored.sessions,
                legacy_import_acknowledgements: Vec::new(),
            })
        }
        VERSION => {
            let Ok(stored) = serde_json::from_slice::<StoredBrowserSessionsV2>(&bytes) else {
                return Ok(LoadedBrowserSessionState::default());
            };
            if validate_sessions(VERSION, &stored.sessions).is_err()
                || validate_acknowledgements(&stored.legacy_import_acknowledgements).is_err()
            {
                return Ok(LoadedBrowserSessionState::default());
            }
            Ok(LoadedBrowserSessionState {
                sessions: stored.sessions,
                legacy_import_acknowledgements: stored.legacy_import_acknowledgements,
            })
        }
        _ => Ok(LoadedBrowserSessionState::default()),
    }
}

#[derive(Deserialize)]
struct StoredBrowserSessionsHeader {
    version: u8,
}

fn validate_sessions(version: u8, sessions: &[StoredBrowserSession]) -> Result<(), String> {
    if !matches!(version, PREVIOUS_FILE_VERSION | VERSION) || sessions.len() > MAX_SESSIONS {
        return Err("browser session storage uses an unsupported shape".to_owned());
    }
    let mut browser_ids = HashSet::new();
    for session in sessions {
        validate_identity(&session.browser_id, &session.workspace_id)?;
        if !browser_ids.insert(session.browser_id.as_str())
            || validated_url(&session.committed_url)? != session.committed_url
            || clean_title(session.title.as_deref()) != session.title
        {
            return Err("browser session storage is invalid".to_owned());
        }
    }
    Ok(())
}

fn validate_acknowledgements(
    acknowledgements: &[StoredLegacyBrowserImportAcknowledgement],
) -> Result<(), String> {
    if acknowledgements.len() > MAX_LEGACY_IMPORT_ACKNOWLEDGEMENTS {
        return Err("browser legacy import storage is too large".to_owned());
    }
    let mut keys = HashSet::new();
    for acknowledgement in acknowledgements {
        validate_identity(&acknowledgement.browser_id, &acknowledgement.workspace_id)?;
        if !keys.insert(acknowledgement_key(acknowledgement)) {
            return Err("browser legacy import storage is invalid".to_owned());
        }
    }
    Ok(())
}

fn acknowledgement_key(
    acknowledgement: &StoredLegacyBrowserImportAcknowledgement,
) -> LegacyBrowserImportKey {
    LegacyBrowserImportKey {
        owner_id: acknowledgement.owner_id.clone(),
        browser_id: acknowledgement.browser_id.clone(),
        workspace_id: acknowledgement.workspace_id.clone(),
    }
}

fn persist_if_initialized(
    state: &BrowserSessionState,
    sessions: &HashMap<String, StoredBrowserSession>,
    acknowledgements: &HashMap<LegacyBrowserImportKey, StoredLegacyBrowserImportAcknowledgement>,
) -> Result<(), String> {
    let (Some(directory), Some(path)) = (&state.directory, &state.path) else {
        return Ok(());
    };
    let mut sessions = sessions.values().cloned().collect::<Vec<_>>();
    sessions.sort_by(|left, right| left.browser_id.cmp(&right.browser_id));
    let mut legacy_import_acknowledgements = acknowledgements.values().cloned().collect::<Vec<_>>();
    legacy_import_acknowledgements.sort_by(|left, right| {
        left.owner_id
            .as_str()
            .cmp(right.owner_id.as_str())
            .then_with(|| left.browser_id.cmp(&right.browser_id))
            .then_with(|| left.workspace_id.cmp(&right.workspace_id))
    });
    let bytes = serde_json::to_vec_pretty(&StoredBrowserSessionsV2 {
        version: VERSION,
        sessions,
        legacy_import_acknowledgements,
    })
    .map_err(|_| "browser session storage could not be encoded".to_owned())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("browser session storage is too large".to_owned());
    }
    write_atomically(directory, path, &bytes)
        .map_err(|_| "browser session storage is unavailable".to_owned())
}

fn write_atomically(directory: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = directory.join(format!(".browser-sessions-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, destination)?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(
        browser_id: &str,
        workspace_id: &str,
        url: Option<&str>,
        title: Option<&str>,
    ) -> LegacyBrowserSession {
        LegacyBrowserSession {
            version: LEGACY_RENDERER_VERSION,
            browser_id: browser_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            url: url.map(str::to_owned),
            title: title.map(str::to_owned),
        }
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn completed_session_reopens_for_the_exact_owner_and_workspace() {
        let private = tempfile::tempdir().unwrap();
        let owner = OwnerId::local();
        let store = BrowserSessionStore::default();
        store.initialize(private.path()).unwrap();
        store
            .commit(
                &owner,
                "browser-1",
                "workspace-1",
                "https://example.com/docs",
                Some("  Documentation   home  "),
            )
            .unwrap();

        let reopened = BrowserSessionStore::default();
        reopened.initialize(private.path()).unwrap();

        assert_eq!(
            reopened
                .recover(&owner, "browser-1", "workspace-1")
                .unwrap(),
            Some(RecoveredBrowserSession {
                url: "https://example.com/docs".to_owned(),
                title: Some("Documentation home".to_owned()),
            })
        );
    }

    #[test]
    fn guessed_workspace_cannot_recover_or_delete_another_session() {
        let store = BrowserSessionStore::default();
        let owner = OwnerId::local();
        store
            .commit(
                &owner,
                "browser-1",
                "workspace-1",
                "https://example.com/",
                None,
            )
            .unwrap();

        assert!(store.recover(&owner, "browser-1", "workspace-2").is_err());
        assert!(store.forget(&owner, "browser-1", "workspace-2").is_err());
        assert!(store
            .recover(&owner, "browser-1", "workspace-1")
            .unwrap()
            .is_some());
    }

    #[test]
    fn legacy_session_imports_once_and_reopens_from_native_state() {
        let private = tempfile::tempdir().unwrap();
        let owner = OwnerId::local();
        let store = BrowserSessionStore::default();
        store.initialize(private.path()).unwrap();

        let imported = store
            .import_legacy(
                &owner,
                "browser-1",
                "workspace-1",
                Some(legacy(
                    "browser-1",
                    "workspace-1",
                    Some("https://example.com/docs"),
                    Some("  Legacy   docs  "),
                )),
            )
            .unwrap();
        assert_eq!(imported.status, LegacyBrowserImportStatus::Imported);
        assert_eq!(imported.url.as_deref(), Some("https://example.com/docs"));
        assert_eq!(imported.title.as_deref(), Some("Legacy docs"));

        let reopened = BrowserSessionStore::default();
        reopened.initialize(private.path()).unwrap();
        let repeated = reopened
            .import_legacy(
                &owner,
                "browser-1",
                "workspace-1",
                Some(legacy(
                    "browser-1",
                    "workspace-1",
                    Some("https://attacker.example/overwrite"),
                    Some("Overwrite"),
                )),
            )
            .unwrap();
        assert_eq!(repeated.status, LegacyBrowserImportStatus::AlreadyHandled);
        assert_eq!(
            reopened
                .recover(&owner, "browser-1", "workspace-1")
                .unwrap()
                .unwrap()
                .url,
            "https://example.com/docs"
        );
    }

    #[test]
    fn native_state_wins_and_cross_workspace_legacy_state_is_discarded() {
        let owner = OwnerId::local();
        let store = BrowserSessionStore::default();
        store
            .commit(
                &owner,
                "browser-1",
                "workspace-1",
                "https://native.example/",
                Some("Native page"),
            )
            .unwrap();

        let kept = store
            .import_legacy(
                &owner,
                "browser-1",
                "workspace-1",
                Some(legacy(
                    "browser-1",
                    "workspace-1",
                    Some("https://legacy.example/"),
                    Some("Legacy page"),
                )),
            )
            .unwrap();
        assert_eq!(kept.status, LegacyBrowserImportStatus::NativeStateKept);
        assert_eq!(kept.url.as_deref(), Some("https://native.example/"));

        let discarded = store
            .import_legacy(
                &owner,
                "browser-1",
                "workspace-2",
                Some(legacy(
                    "browser-1",
                    "workspace-2",
                    Some("https://stale.example/"),
                    Some("Stale page"),
                )),
            )
            .unwrap();
        assert_eq!(discarded.status, LegacyBrowserImportStatus::Discarded);
        assert!(discarded.url.is_none());
        assert_eq!(
            store
                .import_legacy(
                    &owner,
                    "browser-1",
                    "workspace-2",
                    Some(legacy(
                        "browser-1",
                        "workspace-2",
                        Some("https://retry.example/"),
                        None,
                    )),
                )
                .unwrap()
                .status,
            LegacyBrowserImportStatus::AlreadyHandled
        );
        assert_eq!(
            store
                .recover(&owner, "browser-1", "workspace-1")
                .unwrap()
                .unwrap()
                .url,
            "https://native.example/"
        );
    }

    #[test]
    fn malformed_legacy_state_is_acknowledged_without_creating_a_session() {
        let owner = OwnerId::local();
        let store = BrowserSessionStore::default();

        let discarded = store
            .import_legacy(
                &owner,
                "browser-1",
                "workspace-1",
                Some(legacy(
                    "browser-1",
                    "another-workspace",
                    Some("javascript:alert(1)"),
                    Some("Bad\0title"),
                )),
            )
            .unwrap();
        assert_eq!(discarded.status, LegacyBrowserImportStatus::Discarded);
        assert!(store
            .recover(&owner, "browser-1", "workspace-1")
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .import_legacy(&owner, "browser-1", "workspace-1", None)
                .unwrap()
                .status,
            LegacyBrowserImportStatus::AlreadyHandled
        );
    }

    #[test]
    fn explicit_close_acknowledges_migration_and_prevents_resurrection() {
        let private = tempfile::tempdir().unwrap();
        let owner = OwnerId::local();
        let store = BrowserSessionStore::default();
        store.initialize(private.path()).unwrap();
        store
            .commit(
                &owner,
                "browser-1",
                "workspace-1",
                "https://example.com/closed",
                None,
            )
            .unwrap();
        store.forget(&owner, "browser-1", "workspace-1").unwrap();

        let reopened = BrowserSessionStore::default();
        reopened.initialize(private.path()).unwrap();
        let repeated = reopened
            .import_legacy(
                &owner,
                "browser-1",
                "workspace-1",
                Some(legacy(
                    "browser-1",
                    "workspace-1",
                    Some("https://example.com/closed"),
                    None,
                )),
            )
            .unwrap();
        assert_eq!(repeated.status, LegacyBrowserImportStatus::AlreadyHandled);
        assert!(repeated.url.is_none());
        assert!(reopened
            .recover(&owner, "browser-1", "workspace-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn version_one_state_loads_and_upgrades_on_the_next_write() {
        let private = tempfile::tempdir().unwrap();
        let path = private.path().join(FILE_NAME);
        let owner = OwnerId::local();
        let bytes = serde_json::to_vec_pretty(&StoredBrowserSessionsV1 {
            version: PREVIOUS_FILE_VERSION,
            sessions: vec![StoredBrowserSession {
                browser_id: "browser-1".to_owned(),
                owner_id: owner.clone(),
                workspace_id: "workspace-1".to_owned(),
                committed_url: "https://example.com/legacy-native".to_owned(),
                title: Some("Legacy native".to_owned()),
                updated_at: Utc::now(),
            }],
        })
        .unwrap();
        write_private(&path, &bytes);

        let store = BrowserSessionStore::default();
        store.initialize(private.path()).unwrap();
        assert_eq!(
            store
                .recover(&owner, "browser-1", "workspace-1")
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("Legacy native")
        );
        assert_eq!(
            store
                .import_legacy(&owner, "browser-2", "workspace-1", None)
                .unwrap()
                .status,
            LegacyBrowserImportStatus::Discarded
        );

        let stored: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(stored["version"], VERSION);
        assert_eq!(
            stored["legacy_import_acknowledgements"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn invalid_or_oversized_content_is_discarded() {
        let private = tempfile::tempdir().unwrap();
        let path = private.path().join(FILE_NAME);
        write_private(&path, b"not json");
        let store = BrowserSessionStore::default();
        store.initialize(private.path()).unwrap();
        assert!(store
            .recover(&OwnerId::local(), "browser-1", "workspace-1")
            .unwrap()
            .is_none());

        write_private(&path, &vec![b'x'; (MAX_FILE_BYTES + 1) as usize]);
        let reopened = BrowserSessionStore::default();
        reopened.initialize(private.path()).unwrap();
        assert!(reopened
            .recover(&OwnerId::local(), "browser-1", "workspace-1")
            .unwrap()
            .is_none());
    }
}
