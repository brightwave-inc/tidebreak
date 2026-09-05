//! Durable native consent for one owner, workspace, and browser origin scope.
//!
//! Only an explicit native sharing decision writes this store. Browser recovery
//! and renderer preferences cannot create consent. Controllers and pending work
//! remain in the live registry.

use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tidebreak_core::{
    replace_file, sync_directory, BrowserGrantCapability, BrowserOrigin, BrowserOriginScope,
    OwnerId,
};
use uuid::Uuid;

const DIRECTORY: &str = "browser";
const FILE_NAME: &str = "agent-origin-grants.json";
const VERSION: u8 = 1;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_GRANTS: usize = 256;
const MAX_WORKSPACE_ID_CHARS: usize = 200;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BrowserGrant {
    pub(crate) owner_id: OwnerId,
    pub(crate) workspace_id: String,
    pub(crate) scope: BrowserOriginScope,
    pub(crate) capabilities: HashSet<BrowserGrantCapability>,
}

pub(crate) struct BrowserGrantStore {
    directory: PathBuf,
    path: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBrowserGrants {
    version: u8,
    grants: Vec<StoredBrowserGrant>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBrowserGrant {
    owner_id: OwnerId,
    workspace_id: String,
    scope: StoredOriginScope,
    capabilities: Vec<BrowserGrantCapability>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum StoredOriginScope {
    Origin { origin: BrowserOrigin },
    LoopbackWorkspace {},
}

impl From<StoredOriginScope> for BrowserOriginScope {
    fn from(value: StoredOriginScope) -> Self {
        match value {
            StoredOriginScope::Origin { origin } => Self::Origin { origin },
            StoredOriginScope::LoopbackWorkspace {} => Self::LoopbackWorkspace,
        }
    }
}

impl From<&BrowserOriginScope> for StoredOriginScope {
    fn from(value: &BrowserOriginScope) -> Self {
        match value {
            BrowserOriginScope::Origin { origin } => Self::Origin {
                origin: origin.clone(),
            },
            BrowserOriginScope::LoopbackWorkspace => Self::LoopbackWorkspace {},
        }
    }
}

impl BrowserGrantStore {
    pub(crate) fn open(data_dir: &Path) -> Result<(Self, Vec<BrowserGrant>), String> {
        let directory = data_dir.join(DIRECTORY);
        fs::create_dir_all(&directory).map_err(storage_error)?;
        let metadata = fs::symlink_metadata(&directory).map_err(storage_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Browser sharing storage is not a private directory".to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(storage_error)?;
        }
        validate_directory(&directory)?;
        let path = directory.join(FILE_NAME);
        let grants = load_grants(&path)?;
        Ok((Self { directory, path }, grants))
    }

    pub(crate) fn belongs_to(&self, data_dir: &Path) -> bool {
        self.directory == data_dir.join(DIRECTORY)
    }

    pub(crate) fn persist(&self, grants: &[BrowserGrant]) -> Result<(), String> {
        validate_grants(grants)?;
        validate_directory(&self.directory)?;
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) => validate_file(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage_error(error)),
        }
        let mut sorted = grants.iter().collect::<Vec<_>>();
        sorted.sort_by(|left, right| {
            left.owner_id
                .as_str()
                .cmp(right.owner_id.as_str())
                .then_with(|| left.workspace_id.cmp(&right.workspace_id))
                .then_with(|| scope_key(&left.scope).cmp(scope_key(&right.scope)))
        });
        let grants = sorted
            .into_iter()
            .map(|grant| StoredBrowserGrant {
                owner_id: grant.owner_id.clone(),
                workspace_id: grant.workspace_id.clone(),
                scope: (&grant.scope).into(),
                capabilities: [
                    BrowserGrantCapability::BrowserObserveOrigin,
                    BrowserGrantCapability::BrowserControlOrigin,
                    BrowserGrantCapability::BrowserTransferFiles,
                ]
                .into_iter()
                .filter(|capability| grant.capabilities.contains(capability))
                .collect(),
            })
            .collect();
        let bytes = serde_json::to_vec_pretty(&StoredBrowserGrants {
            version: VERSION,
            grants,
        })
        .map_err(|_| "Browser sharing choices could not be encoded".to_owned())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("Browser sharing storage is full".to_owned());
        }
        write_atomically(&self.directory, &self.path, &bytes).map_err(storage_error)
    }
}

fn scope_key(scope: &BrowserOriginScope) -> &str {
    match scope {
        BrowserOriginScope::Origin { origin } => origin.as_str(),
        BrowserOriginScope::LoopbackWorkspace => "",
    }
}

fn validate_grants(grants: &[BrowserGrant]) -> Result<(), String> {
    if grants.len() > MAX_GRANTS {
        return Err("Browser sharing storage is full".to_owned());
    }
    let mut keys = HashSet::new();
    for grant in grants {
        if grant.workspace_id.is_empty()
            || grant.workspace_id.chars().count() > MAX_WORKSPACE_ID_CHARS
            || grant.workspace_id.chars().any(char::is_control)
            || grant.capabilities.is_empty()
            || !keys.insert((&grant.owner_id, grant.workspace_id.as_str(), &grant.scope))
        {
            return Err("Browser sharing storage is invalid".to_owned());
        }
    }
    Ok(())
}

fn validate_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(storage_error)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Browser sharing storage is not a private directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Browser sharing directory has broad permissions".to_owned());
        }
    }
    Ok(())
}

fn validate_file(metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_FILE_BYTES {
        return Err("Browser sharing storage is invalid".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Browser sharing storage has broad permissions".to_owned());
        }
    }
    Ok(())
}

fn load_grants(path: &Path) -> Result<Vec<BrowserGrant>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_file(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(storage_error(error)),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(storage_error)?;
    validate_file(&file.metadata().map_err(storage_error)?)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(storage_error)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("Browser sharing storage is too large".to_owned());
    }
    let stored: StoredBrowserGrants = serde_json::from_slice(&bytes)
        .map_err(|_| "Browser sharing storage is invalid".to_owned())?;
    if stored.version != VERSION || stored.grants.len() > MAX_GRANTS {
        return Err("Browser sharing storage uses an unsupported format".to_owned());
    }
    let mut grants = Vec::with_capacity(stored.grants.len());
    for grant in stored.grants {
        let capabilities = grant.capabilities.iter().copied().collect::<HashSet<_>>();
        if capabilities.len() != grant.capabilities.len() {
            return Err("Browser sharing storage is invalid".to_owned());
        }
        grants.push(BrowserGrant {
            owner_id: grant.owner_id,
            workspace_id: grant.workspace_id,
            scope: grant.scope.into(),
            capabilities,
        });
    }
    validate_grants(&grants)?;
    Ok(grants)
}

fn write_atomically(directory: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = directory.join(format!(".browser-grants-{}.tmp", Uuid::new_v4()));
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

fn storage_error(_: io::Error) -> String {
    "Could not save browser sharing choices. Check private storage and try again.".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(workspace_id: &str, origin: &str) -> BrowserGrant {
        BrowserGrant {
            owner_id: OwnerId::local(),
            workspace_id: workspace_id.to_owned(),
            scope: BrowserOriginScope::Origin {
                origin: BrowserOrigin::parse(origin).unwrap(),
            },
            capabilities: HashSet::from([BrowserGrantCapability::BrowserControlOrigin]),
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
    fn consent_round_trips_with_private_permissions_and_no_runtime_state() {
        let private = tempfile::tempdir().unwrap();
        let (store, empty) = BrowserGrantStore::open(private.path()).unwrap();
        assert!(empty.is_empty());
        let choices = vec![grant("workspace-1", "https://example.com")];
        store.persist(&choices).unwrap();
        let (_, restored) = BrowserGrantStore::open(private.path()).unwrap();
        assert_eq!(restored, choices);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&store.path).unwrap()).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 2);
        assert_eq!(json["grants"][0].as_object().unwrap().len(), 4);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&store.directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&store.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 1);
    }

    #[test]
    fn malformed_and_ambiguous_consent_never_loads_partially() {
        let private = tempfile::tempdir().unwrap();
        let (store, _) = BrowserGrantStore::open(private.path()).unwrap();
        store
            .persist(&[grant("workspace-1", "https://example.com")])
            .unwrap();
        let valid: serde_json::Value =
            serde_json::from_slice(&fs::read(&store.path).unwrap()).unwrap();
        let mut cases = vec![
            serde_json::json!(null),
            serde_json::json!({"version": 2, "grants": []}),
        ];
        for (field, value) in [
            ("owner_id", serde_json::json!("")),
            ("workspace_id", serde_json::json!("bad\nworkspace")),
            ("capabilities", serde_json::json!([])),
            (
                "capabilities",
                serde_json::json!(["browser_control_origin", "browser_control_origin"]),
            ),
            ("capabilities", serde_json::json!(["all"])),
            (
                "scope",
                serde_json::json!({"kind": "origin", "origin": "https://example.com/private"}),
            ),
            (
                "scope",
                serde_json::json!({"kind": "loopback_workspace", "origin": "https://example.com"}),
            ),
            ("controller", serde_json::json!("agent")),
        ] {
            let mut invalid = valid.clone();
            invalid["grants"][0][field] = value;
            cases.push(invalid);
        }
        let mut duplicate = valid.clone();
        duplicate["grants"]
            .as_array_mut()
            .unwrap()
            .push(valid["grants"][0].clone());
        cases.push(duplicate);
        for invalid in cases {
            write_private(&store.path, &serde_json::to_vec(&invalid).unwrap());
            assert!(
                BrowserGrantStore::open(private.path()).is_err(),
                "accepted {invalid}"
            );
        }
        write_private(&store.path, b"{truncated");
        assert!(BrowserGrantStore::open(private.path()).is_err());
        write_private(&store.path, &vec![b' '; MAX_FILE_BYTES as usize + 1]);
        assert!(BrowserGrantStore::open(private.path()).is_err());
    }

    #[test]
    fn failed_write_does_not_publish_a_choice_or_leave_temporary_files() {
        let private = tempfile::tempdir().unwrap();
        let (store, _) = BrowserGrantStore::open(private.path()).unwrap();
        fs::create_dir(&store.path).unwrap();
        assert!(store
            .persist(&[grant("workspace-1", "https://example.com")])
            .is_err());
        assert!(store.path.is_dir());
        assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 1);
    }

    #[test]
    fn grant_count_and_workspace_size_are_bounded_before_writing() {
        let private = tempfile::tempdir().unwrap();
        let (store, _) = BrowserGrantStore::open(private.path()).unwrap();
        let grants = (0..=MAX_GRANTS)
            .map(|index| grant(&format!("workspace-{index}"), "https://example.com"))
            .collect::<Vec<_>>();
        assert!(store.persist(&grants).is_err());
        assert!(store
            .persist(&[grant(
                &"w".repeat(MAX_WORKSPACE_ID_CHARS + 1),
                "https://example.com"
            )])
            .is_err());
        assert!(!store.path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn directory_permissions_and_write_failures_do_not_replace_consent() {
        use std::os::unix::fs::PermissionsExt as _;
        let private = tempfile::tempdir().unwrap();
        let (store, _) = BrowserGrantStore::open(private.path()).unwrap();
        store
            .persist(&[grant("workspace-1", "https://example.com")])
            .unwrap();
        let before = fs::read(&store.path).unwrap();
        fs::set_permissions(&store.directory, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(store
            .persist(&[])
            .unwrap_err()
            .contains("broad permissions"));
        assert_eq!(fs::read(&store.path).unwrap(), before);
        fs::set_permissions(&store.directory, fs::Permissions::from_mode(0o500)).unwrap();
        assert!(store.persist(&[]).is_err());
        assert_eq!(fs::read(&store.path).unwrap(), before);
        fs::set_permissions(&store.directory, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_broad_file_permissions_cannot_supply_consent() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};
        let private = tempfile::tempdir().unwrap();
        let (store, _) = BrowserGrantStore::open(private.path()).unwrap();
        store
            .persist(&[grant("workspace-1", "https://example.com")])
            .unwrap();
        fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(BrowserGrantStore::open(private.path()).is_err());
        assert!(store.persist(&[]).is_err());
        fs::remove_file(&store.path).unwrap();
        let target = private.path().join("outside.json");
        write_private(&target, b"{\"version\":1,\"grants\":[]}");
        symlink(&target, &store.path).unwrap();
        assert!(BrowserGrantStore::open(private.path()).is_err());
        assert!(store.persist(&[]).is_err());
        fs::remove_file(&store.path).unwrap();
        fs::remove_dir(&store.directory).unwrap();
        let outside = private.path().join("outside-directory");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &store.directory).unwrap();
        assert!(BrowserGrantStore::open(private.path()).is_err());
    }
}
