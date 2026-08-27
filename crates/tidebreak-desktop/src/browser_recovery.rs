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
use tidebreak_core::OwnerId;
use url::Url;
use uuid::Uuid;

const FILE_NAME: &str = "browser-sessions.json";
const VERSION: u8 = 1;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SESSIONS: usize = 256;
const MAX_BROWSER_ID_CHARS: usize = 80;
const MAX_WORKSPACE_ID_CHARS: usize = 200;
const MAX_URL_CHARS: usize = 8_192;
const MAX_TITLE_CHARS: usize = 160;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveredBrowserSession {
    pub(crate) url: String,
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBrowserSessions {
    version: u8,
    sessions: Vec<StoredBrowserSession>,
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

impl BrowserSessionStore {
    pub(crate) fn initialize(&self, data_dir: &Path) -> Result<(), String> {
        fs::create_dir_all(data_dir)
            .map_err(|_| "could not create private browser state".to_owned())?;
        let path = data_dir.join(FILE_NAME);
        let sessions = load_sessions(&path)?;
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
        state.sessions = sessions
            .into_iter()
            .map(|session| (session.browser_id.clone(), session))
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
        persist_if_initialized(&state, &next)?;
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
        persist_if_initialized(&state, &next)?;
        state.sessions = next;
        Ok(true)
    }

    pub(crate) fn forget(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        validate_identity(browser_id, workspace_id)?;
        let mut state = self.lock();
        let Some(existing) = state.sessions.get(browser_id) else {
            return Ok(());
        };
        ensure_binding(existing, owner_id, workspace_id)?;
        let mut next = state.sessions.clone();
        next.remove(browser_id);
        persist_if_initialized(&state, &next)?;
        state.sessions = next;
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
    let title: String = value?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
    (!title.is_empty()).then_some(title)
}

fn load_sessions(path: &Path) -> Result<Vec<StoredBrowserSession>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
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
        return Ok(Vec::new());
    }
    let bytes = fs::read(path).map_err(|_| "browser session storage is unavailable".to_owned())?;
    let Ok(stored) = serde_json::from_slice::<StoredBrowserSessions>(&bytes) else {
        return Ok(Vec::new());
    };
    if validate_sessions(&stored).is_err() {
        return Ok(Vec::new());
    }
    Ok(stored.sessions)
}

fn validate_sessions(stored: &StoredBrowserSessions) -> Result<(), String> {
    if stored.version != VERSION || stored.sessions.len() > MAX_SESSIONS {
        return Err("browser session storage uses an unsupported shape".to_owned());
    }
    let mut browser_ids = HashSet::new();
    for session in &stored.sessions {
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

fn persist_if_initialized(
    state: &BrowserSessionState,
    sessions: &HashMap<String, StoredBrowserSession>,
) -> Result<(), String> {
    let (Some(directory), Some(path)) = (&state.directory, &state.path) else {
        return Ok(());
    };
    let mut sessions = sessions.values().cloned().collect::<Vec<_>>();
    sessions.sort_by(|left, right| left.browser_id.cmp(&right.browser_id));
    let bytes = serde_json::to_vec_pretty(&StoredBrowserSessions {
        version: VERSION,
        sessions,
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

#[cfg(unix)]
fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    fs::File::open(directory)?.sync_all()
}

#[cfg(windows)]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
