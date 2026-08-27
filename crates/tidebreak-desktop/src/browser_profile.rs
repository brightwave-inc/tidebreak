//! Native-only lifecycle for Tidebreak's managed development-browser profile.
//!
//! The renderer receives an opaque profile id only as session metadata. The
//! engine data-store identifier, website records needed by older WebKit, and
//! every byte written by the engine remain in this native app profile.

use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};
use tidebreak_core::OwnerId;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use url::Url;
use uuid::Uuid;

const DIRECTORY: &str = "browser";
const MANIFEST_FILE: &str = "managed-profiles.json";
const VERSION: u8 = 1;
const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const MAX_PROFILES: usize = 32;
const MAX_WEBSITE_HOSTS: usize = 4_096;
const MAX_WEBSITE_HOST_CHARS: usize = 253;

/// The browser-only named store already shipped on main before profile
/// manifests existed. The first local-owner allocation adopts this exact
/// Tidebreak store so an application update does not discard its cookies.
/// Once adopted, the manifest permanently retires this bootstrap identity and
/// reset allocates a fresh UUID instead of selecting it again.
const INITIAL_LOCAL_DATA_STORE_IDENTIFIER: [u8; 16] = [
    0x74, 0x69, 0x64, 0x65, 0x62, 0x72, 0x65, 0x61, 0x6b, 0x2d, 0x62, 0x72, 0x6f, 0x77, 0x73, 0x65,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedBrowserProfile {
    owner_id: OwnerId,
    profile_id: Uuid,
    #[cfg(any(target_os = "macos", test))]
    data_store_identifier: Uuid,
    #[cfg(any(target_os = "macos", test))]
    website_hosts: BTreeSet<String>,
}

impl ManagedBrowserProfile {
    pub(crate) fn owner_id(&self) -> &OwnerId {
        &self.owner_id
    }

    pub(crate) fn profile_id(&self) -> String {
        self.profile_id.to_string()
    }

    /// Stable native engine identity. This value never crosses Tauri IPC.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn data_store_identifier(&self) -> [u8; 16] {
        *self.data_store_identifier.as_bytes()
    }

    /// Website-record selectors used only by the targeted pre-macOS 14 reset.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn website_hosts(&self) -> &BTreeSet<String> {
        &self.website_hosts
    }
}

#[derive(Clone)]
pub(crate) struct BrowserProfileStore {
    inner: Arc<Mutex<BrowserProfileState>>,
    lifecycle: Arc<AsyncMutex<()>>,
}

struct BrowserProfileState {
    directory: PathBuf,
    path: PathBuf,
    initial_local_store_available: bool,
    profiles: Vec<StoredBrowserProfile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBrowserProfiles {
    version: u8,
    #[serde(default = "default_initial_local_store_available")]
    initial_local_store_available: bool,
    profiles: Vec<StoredBrowserProfile>,
}

fn default_initial_local_store_available() -> bool {
    true
}

struct LoadedBrowserProfiles {
    initial_local_store_available: bool,
    profiles: Vec<StoredBrowserProfile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBrowserProfile {
    owner_id: OwnerId,
    profile_id: Uuid,
    data_store_identifier: Uuid,
    website_hosts: BTreeSet<String>,
}

impl StoredBrowserProfile {
    fn managed(&self) -> ManagedBrowserProfile {
        ManagedBrowserProfile {
            owner_id: self.owner_id.clone(),
            profile_id: self.profile_id,
            #[cfg(any(target_os = "macos", test))]
            data_store_identifier: self.data_store_identifier,
            #[cfg(any(target_os = "macos", test))]
            website_hosts: self.website_hosts.clone(),
        }
    }
}

impl BrowserProfileStore {
    pub(crate) fn open(data_dir: &Path) -> Result<Self, String> {
        let directory = data_dir.join(DIRECTORY);
        fs::create_dir_all(&directory).map_err(profile_storage_error)?;
        let metadata = fs::symlink_metadata(&directory).map_err(profile_storage_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("browser profile storage is invalid".to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(profile_storage_error)?;
        }
        let path = directory.join(MANIFEST_FILE);
        let loaded = load_profiles(&path)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(BrowserProfileState {
                directory,
                path,
                initial_local_store_available: loaded.initial_local_store_available,
                profiles: loaded.profiles,
            })),
            lifecycle: Arc::new(AsyncMutex::new(())),
        })
    }

    /// Serialize native profile allocation and reset. A reset therefore
    /// cannot delete a store while another command is creating a view on it.
    pub(crate) async fn lock_lifecycle(&self) -> OwnedMutexGuard<()> {
        Arc::clone(&self.lifecycle).lock_owned().await
    }

    pub(crate) fn get_or_create(
        &self,
        owner_id: &OwnerId,
    ) -> Result<ManagedBrowserProfile, String> {
        let mut state = self.lock();
        if let Some(profile) = state
            .profiles
            .iter()
            .find(|profile| profile.owner_id == *owner_id)
        {
            return Ok(profile.managed());
        }
        if state.profiles.len() >= MAX_PROFILES {
            return Err("browser profile storage is full".to_owned());
        }

        let initial_store = Uuid::from_bytes(INITIAL_LOCAL_DATA_STORE_IDENTIFIER);
        let profile_id = fresh_uuid(
            state
                .profiles
                .iter()
                .flat_map(|profile| [profile.profile_id, profile.data_store_identifier])
                .chain(std::iter::once(initial_store)),
        );
        let adopts_initial_store = owner_id.is_local() && state.initial_local_store_available;
        let data_store_identifier = if adopts_initial_store {
            initial_store
        } else {
            fresh_uuid(
                state
                    .profiles
                    .iter()
                    .flat_map(|profile| [profile.profile_id, profile.data_store_identifier])
                    .chain([profile_id, initial_store]),
            )
        };
        let profile = StoredBrowserProfile {
            owner_id: owner_id.clone(),
            profile_id,
            data_store_identifier,
            website_hosts: BTreeSet::new(),
        };
        let mut next = state.profiles.clone();
        next.push(profile.clone());
        next.sort_by(|left, right| left.owner_id.as_str().cmp(right.owner_id.as_str()));
        let initial_local_store_available =
            state.initial_local_store_available && !adopts_initial_store;
        persist_profiles(
            &state.directory,
            &state.path,
            initial_local_store_available,
            &next,
        )?;
        state.initial_local_store_available = initial_local_store_available;
        state.profiles = next;
        Ok(profile.managed())
    }

    /// Resolve the exact owner/profile pair derived by the native session
    /// registry. A guessed id and another owner's real id are indistinguishable.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn resolve(
        &self,
        owner_id: &OwnerId,
        profile_id: &str,
    ) -> Result<ManagedBrowserProfile, String> {
        let profile_id =
            Uuid::parse_str(profile_id).map_err(|_| "browser profile is unavailable".to_owned())?;
        self.lock()
            .profiles
            .iter()
            .find(|profile| profile.owner_id == *owner_id && profile.profile_id == profile_id)
            .map(StoredBrowserProfile::managed)
            .ok_or_else(|| "browser profile is unavailable".to_owned())
    }

    /// Record only the normalized website host, never a URL, path, query, or
    /// page content. Older WebKit groups removable records by site/domain.
    pub(crate) fn record_url(
        &self,
        owner_id: &OwnerId,
        profile_id: &str,
        url: &Url,
    ) -> Result<(), String> {
        let host = website_host(url)?;
        let profile_id =
            Uuid::parse_str(profile_id).map_err(|_| "browser profile is unavailable".to_owned())?;
        let mut state = self.lock();
        let Some(index) = state
            .profiles
            .iter()
            .position(|profile| profile.owner_id == *owner_id && profile.profile_id == profile_id)
        else {
            return Err("browser profile is unavailable".to_owned());
        };
        if state.profiles[index].website_hosts.contains(&host) {
            return Ok(());
        }
        if state.profiles[index].website_hosts.len() >= MAX_WEBSITE_HOSTS {
            return Err("browser profile has too many website records".to_owned());
        }

        let mut next = state.profiles.clone();
        next[index].website_hosts.insert(host);
        persist_profiles(
            &state.directory,
            &state.path,
            state.initial_local_store_available,
            &next,
        )?;
        state.profiles = next;
        Ok(())
    }

    /// Forget only the exact native-managed profile after its engine data has
    /// been deleted. No path or engine identifier is accepted from the caller.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn forget(&self, owner_id: &OwnerId, profile_id: &str) -> Result<(), String> {
        let profile_id =
            Uuid::parse_str(profile_id).map_err(|_| "browser profile is unavailable".to_owned())?;
        let mut state = self.lock();
        let Some(index) = state
            .profiles
            .iter()
            .position(|profile| profile.owner_id == *owner_id && profile.profile_id == profile_id)
        else {
            return Err("browser profile is unavailable".to_owned());
        };
        let mut next = state.profiles.clone();
        next.remove(index);
        persist_profiles(
            &state.directory,
            &state.path,
            state.initial_local_store_available,
            &next,
        )?;
        state.profiles = next;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, BrowserProfileState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn fresh_uuid(existing: impl IntoIterator<Item = Uuid>) -> Uuid {
    let existing = existing.into_iter().collect::<HashSet<_>>();
    loop {
        let candidate = Uuid::new_v4();
        if !candidate.is_nil() && !existing.contains(&candidate) {
            return candidate;
        }
    }
}

fn website_host(url: &Url) -> Result<String, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("browser profile accepts only HTTP and HTTPS websites".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "browser website has no host".to_owned())?;
    normalize_website_host(host).ok_or_else(|| "browser website host is not valid".to_owned())
}

pub(crate) fn normalize_website_host(host: &str) -> Option<String> {
    let host = host
        .trim()
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    valid_website_host(&host).then_some(host)
}

fn valid_website_host(host: &str) -> bool {
    !host.is_empty()
        && host.chars().count() <= MAX_WEBSITE_HOST_CHARS
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'_'))
}

fn load_profiles(path: &Path) -> Result<LoadedBrowserProfiles, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedBrowserProfiles {
                initial_local_store_available: true,
                profiles: Vec::new(),
            });
        }
        Err(error) => return Err(profile_storage_error(error)),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err("browser profile storage is invalid".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("browser profile storage has broad permissions".to_owned());
        }
    }
    let bytes = fs::read(path).map_err(profile_storage_error)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("browser profile storage is invalid".to_owned());
    }
    let stored: StoredBrowserProfiles = serde_json::from_slice(&bytes)
        .map_err(|_| "browser profile storage is invalid".to_owned())?;
    validate_profiles(&stored)?;
    let initial_store = Uuid::from_bytes(INITIAL_LOCAL_DATA_STORE_IDENTIFIER);
    let initial_local_store_available = stored.initial_local_store_available
        && !stored.profiles.iter().any(|profile| {
            profile.owner_id.is_local() || profile.data_store_identifier == initial_store
        });
    Ok(LoadedBrowserProfiles {
        initial_local_store_available,
        profiles: stored.profiles,
    })
}

fn validate_profiles(stored: &StoredBrowserProfiles) -> Result<(), String> {
    if stored.version != VERSION || stored.profiles.len() > MAX_PROFILES {
        return Err("browser profile storage uses an unsupported shape".to_owned());
    }
    let initial_store = Uuid::from_bytes(INITIAL_LOCAL_DATA_STORE_IDENTIFIER);
    let mut owners = HashSet::new();
    let mut identifiers = HashSet::new();
    for profile in &stored.profiles {
        if !owners.insert(profile.owner_id.as_str())
            || profile.profile_id.is_nil()
            || profile.data_store_identifier.is_nil()
            || profile.profile_id == initial_store
            || (profile.data_store_identifier == initial_store && !profile.owner_id.is_local())
            || !identifiers.insert(profile.profile_id)
            || !identifiers.insert(profile.data_store_identifier)
            || profile.website_hosts.len() > MAX_WEBSITE_HOSTS
            || profile
                .website_hosts
                .iter()
                .any(|host| normalize_website_host(host).as_deref() != Some(host.as_str()))
        {
            return Err("browser profile storage is invalid".to_owned());
        }
    }
    Ok(())
}

fn persist_profiles(
    directory: &Path,
    destination: &Path,
    initial_local_store_available: bool,
    profiles: &[StoredBrowserProfile],
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&StoredBrowserProfiles {
        version: VERSION,
        initial_local_store_available,
        profiles: profiles.to_vec(),
    })
    .map_err(|_| "browser profile storage could not be encoded".to_owned())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("browser profile storage is too large".to_owned());
    }
    write_atomically(directory, destination, &bytes).map_err(profile_storage_error)
}

fn write_atomically(directory: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = directory.join(format!(".managed-profiles-{}.tmp", Uuid::new_v4()));
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

fn profile_storage_error(_error: impl std::fmt::Display) -> String {
    "browser profile storage is unavailable".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_owner_reopens_the_same_persistent_profile() {
        let private = tempfile::tempdir().unwrap();
        let owner = OwnerId::local();
        let first = BrowserProfileStore::open(private.path())
            .unwrap()
            .get_or_create(&owner)
            .unwrap();

        let reopened = BrowserProfileStore::open(private.path())
            .unwrap()
            .get_or_create(&owner)
            .unwrap();

        assert_eq!(reopened.profile_id(), first.profile_id());
        assert_eq!(
            reopened.data_store_identifier(),
            first.data_store_identifier()
        );
        assert_eq!(
            first.data_store_identifier(),
            INITIAL_LOCAL_DATA_STORE_IDENTIFIER
        );
    }

    #[test]
    fn owners_are_isolated_and_cannot_redeem_each_others_profile_ids() {
        let private = tempfile::tempdir().unwrap();
        let store = BrowserProfileStore::open(private.path()).unwrap();
        let alice = OwnerId::new("alice").unwrap();
        let bob = OwnerId::new("bob").unwrap();
        let alice_profile = store.get_or_create(&alice).unwrap();
        let bob_profile = store.get_or_create(&bob).unwrap();

        assert_ne!(alice_profile.profile_id(), bob_profile.profile_id());
        assert_ne!(
            alice_profile.data_store_identifier(),
            bob_profile.data_store_identifier()
        );
        assert!(store.resolve(&bob, &alice_profile.profile_id()).is_err());
        assert!(store.forget(&bob, &alice_profile.profile_id()).is_err());
        assert_eq!(
            store
                .resolve(&alice, &alice_profile.profile_id())
                .unwrap()
                .profile_id(),
            alice_profile.profile_id()
        );
    }

    #[test]
    fn guessed_profile_ids_are_refused_without_changing_the_real_profile() {
        let private = tempfile::tempdir().unwrap();
        let store = BrowserProfileStore::open(private.path()).unwrap();
        let owner = OwnerId::local();
        let profile = store.get_or_create(&owner).unwrap();

        assert!(store.forget(&owner, &Uuid::new_v4().to_string()).is_err());
        assert_eq!(
            store
                .resolve(&owner, &profile.profile_id())
                .unwrap()
                .profile_id(),
            profile.profile_id()
        );
    }

    #[test]
    fn successful_reset_allocates_a_fresh_profile_next_time() {
        let private = tempfile::tempdir().unwrap();
        let store = BrowserProfileStore::open(private.path()).unwrap();
        let owner = OwnerId::local();
        let first = store.get_or_create(&owner).unwrap();
        store.forget(&owner, &first.profile_id()).unwrap();

        let fresh = store.get_or_create(&owner).unwrap();

        assert_ne!(fresh.profile_id(), first.profile_id());
        assert_ne!(fresh.data_store_identifier(), first.data_store_identifier());
        assert_ne!(
            fresh.data_store_identifier(),
            INITIAL_LOCAL_DATA_STORE_IDENTIFIER
        );
        drop(store);

        let reopened = BrowserProfileStore::open(private.path())
            .unwrap()
            .get_or_create(&owner)
            .unwrap();
        assert_eq!(reopened.profile_id(), fresh.profile_id());
        assert_eq!(
            reopened.data_store_identifier(),
            fresh.data_store_identifier()
        );
    }

    #[test]
    fn only_normalized_website_hosts_enter_native_reset_metadata() {
        let private = tempfile::tempdir().unwrap();
        let store = BrowserProfileStore::open(private.path()).unwrap();
        let owner = OwnerId::local();
        let profile = store.get_or_create(&owner).unwrap();
        store
            .record_url(
                &owner,
                &profile.profile_id(),
                &Url::parse("https://Docs.Example.COM/private?token=secret").unwrap(),
            )
            .unwrap();

        let reopened = BrowserProfileStore::open(private.path())
            .unwrap()
            .resolve(&owner, &profile.profile_id())
            .unwrap();
        assert_eq!(
            reopened.website_hosts(),
            &BTreeSet::from(["docs.example.com".to_owned()])
        );
        let manifest =
            fs::read_to_string(private.path().join(DIRECTORY).join(MANIFEST_FILE)).unwrap();
        assert!(manifest.contains("docs.example.com"));
        assert!(!manifest.contains("/private"));
        assert!(!manifest.contains("token"));
        assert!(!manifest.contains("secret"));
    }

    #[test]
    fn another_owner_cannot_consume_the_local_bootstrap_store() {
        let private = tempfile::tempdir().unwrap();
        let alice = OwnerId::new("alice").unwrap();
        let first = BrowserProfileStore::open(private.path()).unwrap();
        let alice_profile = first.get_or_create(&alice).unwrap();
        assert_ne!(
            alice_profile.data_store_identifier(),
            INITIAL_LOCAL_DATA_STORE_IDENTIFIER
        );
        drop(first);

        let local = BrowserProfileStore::open(private.path())
            .unwrap()
            .get_or_create(&OwnerId::local())
            .unwrap();
        assert_eq!(
            local.data_store_identifier(),
            INITIAL_LOCAL_DATA_STORE_IDENTIFIER
        );
    }

    #[test]
    fn manifest_rejects_identifier_collisions_across_profile_and_store_kinds() {
        let shared = Uuid::new_v4();
        let stored = StoredBrowserProfiles {
            version: VERSION,
            initial_local_store_available: true,
            profiles: vec![
                StoredBrowserProfile {
                    owner_id: OwnerId::new("alice").unwrap(),
                    profile_id: Uuid::new_v4(),
                    data_store_identifier: shared,
                    website_hosts: BTreeSet::new(),
                },
                StoredBrowserProfile {
                    owner_id: OwnerId::new("bob").unwrap(),
                    profile_id: shared,
                    data_store_identifier: Uuid::new_v4(),
                    website_hosts: BTreeSet::new(),
                },
            ],
        };

        assert!(validate_profiles(&stored).is_err());
    }

    #[test]
    fn legacy_manifest_without_bootstrap_flag_remains_readable_and_retires_it() {
        let private = tempfile::tempdir().unwrap();
        let directory = private.path().join(DIRECTORY);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(MANIFEST_FILE);
        let profile_id = Uuid::new_v4();
        let data_store_identifier = Uuid::new_v4();
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "version": VERSION,
            "profiles": [{
                "owner_id": OwnerId::LOCAL,
                "profile_id": profile_id,
                "data_store_identifier": data_store_identifier,
                "website_hosts": [],
            }],
        }))
        .unwrap();
        write_atomically(&directory, &path, &bytes).unwrap();

        let store = BrowserProfileStore::open(private.path()).unwrap();
        let legacy = store.get_or_create(&OwnerId::local()).unwrap();
        assert_eq!(legacy.profile_id(), profile_id.to_string());
        assert_eq!(
            legacy.data_store_identifier(),
            *data_store_identifier.as_bytes()
        );

        store
            .forget(&OwnerId::local(), &legacy.profile_id())
            .unwrap();
        let fresh = store.get_or_create(&OwnerId::local()).unwrap();
        assert_ne!(
            fresh.data_store_identifier(),
            INITIAL_LOCAL_DATA_STORE_IDENTIFIER
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_profile_directory_is_refused() {
        use std::os::unix::fs::symlink;

        let private = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), private.path().join(DIRECTORY)).unwrap();

        assert!(BrowserProfileStore::open(private.path()).is_err());
    }
}
